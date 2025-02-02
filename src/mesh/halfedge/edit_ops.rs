use std::collections::BTreeSet;

use anyhow::{anyhow, bail};
use smallvec::SmallVec;

use crate::prelude::*;

/// This map is used in many operations when halfedges are still being built.
/// Sometimes we need to keep this information to locate twins, and using
/// `halfedge_to` won't work because we can't cycle the edges around a vertex
/// fan until twins are assigned.
type PairToHalfEdge = std::collections::HashMap<(VertexId, VertexId), HalfEdgeId>;

/// Given a list of vertices, forms a face with all of them. To call this
/// function, the vertices should be in the right winding order and must all be
/// part of the same boundary. If not, it will panic or may produce wrong results.
fn add_face(
    mesh: &mut HalfEdgeMesh,
    vertices: &[VertexId],
    pair_to_halfedge: &mut PairToHalfEdge,
) -> FaceId {
    let mut halfedges = SVec::new();

    let f = mesh.alloc_face(None);

    for (&v, &v2) in vertices.iter().circular_tuple_windows() {
        // Some vertices may already be connected by an edge. We should avoid
        // creating halfedges for those.
        let h = if let Some(&h) = pair_to_halfedge.get(&(v, v2)) {
            // The halfedge may exist, but the face could've changed.
            mesh[h].face = Some(f);
            h
        } else {
            mesh.alloc_halfedge(HalfEdge {
                vertex: Some(v),
                face: Some(f),
                twin: None, // TBD
                next: None, // TBD
            })
        };
        pair_to_halfedge.insert((v, v2), h);
        halfedges.push(h);
        mesh[v].halfedge = Some(h);
    }

    for (&ha, &hb) in halfedges.iter().circular_tuple_windows() {
        mesh[ha].next = Some(hb);
    }

    mesh[f].halfedge = Some(halfedges[0]);

    // For each pair of vertices a,b and the halfedge that goes from a to b,
    // a_h_b, we attempt to find its twin, that is, the edge in the mesh that
    // goes from b to a. If found, we link it to the one we created.
    //
    // NOTE: Both the halfedge and its twin may already exist and be linked. In
    // that case, they are simply reassigned. If the twin does not exist,
    // nothing happens, it may be linked later as part of anoter add_face
    // operation.
    for ((&a, &b), h_a_b) in vertices.iter().circular_tuple_windows().zip(halfedges) {
        if let Some(&h_b_a) = pair_to_halfedge.get(&(b, a)) {
            mesh[h_b_a].twin = Some(h_a_b);
            mesh[h_a_b].twin = Some(h_b_a);
        }
    }

    f
}

pub fn extrude_face_connectivity(
    mesh: &mut HalfEdgeMesh,
    face_id: FaceId,
    position_delta: Vec3,
) -> (SVec<FaceId>, FaceId) {
    let vertices = mesh.at_face(face_id).vertices().unwrap();
    let halfedges = mesh.at_face(face_id).halfedges().unwrap();

    let mut new_vertices = SVec::new();
    for &v in vertices.iter() {
        let pos = mesh.vertex_position(v);
        new_vertices.push(mesh.alloc_vertex(pos + position_delta, None));
    }

    // NOTE: It's important to initialize this structure, or some halfedges
    // would get duplicated.
    let mut pair_to_halfedge: PairToHalfEdge = vertices
        .iter()
        .cloned()
        .circular_tuple_windows()
        .zip(halfedges.iter().cloned())
        .collect();

    let mut side_faces = SVec::new();

    // v1->v2 is the direction of the existing halfedges. We need to follow that
    // same direction to preserve mesh orientation.
    for ((&v1, &v1_new), (&v2, &v2_new)) in vertices
        .iter()
        .zip(new_vertices.iter())
        .circular_tuple_windows()
    {
        side_faces.push(add_face(
            mesh,
            &[v1, v2, v2_new, v1_new],
            &mut pair_to_halfedge,
        ));
    }

    // TODO: Maybe reuse the old face?
    let front_face = add_face(mesh, new_vertices.as_slice(), &mut pair_to_halfedge);

    mesh.faces.remove(face_id.0);

    #[cfg(debug_assertions)]
    for halfedge in halfedges {
        debug_assert!(
            mesh[halfedge].face.unwrap() != face_id,
            "None of the original halfedges should point to the old face"
        );
    }

    for vertex in mesh.at_face(front_face).vertices().unwrap().iter() {
        mesh.add_debug_vertex(*vertex, DebugMark::new("ex", egui::Color32::RED));
    }

    (side_faces, front_face)
}

pub const ORANGE: egui::Color32 = egui::Color32::from_rgb(200, 200, 0);

#[allow(non_snake_case)]
pub fn split_vertex(
    mesh: &mut HalfEdgeMesh,
    v: VertexId,
    v_l: VertexId,
    v_r: VertexId,
    delta: Vec3,
    dbg: bool,
) -> Result<VertexId> {
    let v_pos = mesh.vertex_position(v);

    // Find h_L and h_R
    let h_r_v = mesh.at_vertex(v_r).halfedge_to(v).try_end()?;
    let h_v_r = mesh.at_halfedge(h_r_v).twin().end();
    let h_v_l = mesh.at_vertex(v).halfedge_to(v_l).try_end()?;
    let h_l_v = mesh.at_halfedge(h_v_l).twin().end();

    if dbg {
        mesh.add_debug_halfedge(h_r_v, DebugMark::new("h_R", ORANGE));
        mesh.add_debug_halfedge(h_v_l, DebugMark::new("h_L", ORANGE));
    }

    // Get all the halfedges edges connected to v starting at v_r and ending at
    // v_l in clockwise order
    let (incoming_hs, outgoing_hs) = {
        let incoming = mesh.at_vertex(v).incoming_halfedges()?;
        let outgoing = mesh.at_vertex(v).outgoing_halfedges()?;

        let h_incoming_start = incoming.iter().position(|x| *x == h_r_v).unwrap();
        let h_incoming_end = incoming.iter().position(|x| *x == h_l_v).unwrap();
        let h_incoming_end = if h_incoming_end < h_incoming_start {
            h_incoming_end + incoming.len()
        } else {
            h_incoming_end
        };

        if dbg {
            dbg!(incoming.iter().position(|x| *x == h_r_v).unwrap());
            dbg!(incoming.iter().position(|x| *x == h_l_v).unwrap());
        }

        let incoming_hs: SVec<HalfEdgeId> = (h_incoming_start + 1..h_incoming_end)
            .map(|x| x % incoming.len())
            .map(|idx| incoming[idx])
            .collect();

        if dbg {
            for &h in &incoming_hs {
                mesh.add_debug_halfedge(h, DebugMark::new("", egui::Color32::BLUE));
            }
        }

        let h_outgoing_start = outgoing.iter().position(|x| *x == h_v_r).unwrap();
        let h_outgoing_end = outgoing.iter().position(|x| *x == h_v_l).unwrap();
        let h_outgoing_end = if h_outgoing_end < h_outgoing_start {
            h_outgoing_end + outgoing.len()
        } else {
            h_outgoing_end
        };
        let outgoing_hs: SVec<HalfEdgeId> = (h_outgoing_start + 1..h_outgoing_end)
            .map(|x| x % outgoing.len())
            .map(|idx| outgoing[idx])
            .collect();

        if dbg {
            for &h in &outgoing_hs {
                mesh.add_debug_halfedge(h, DebugMark::new("", egui::Color32::RED));
            }
        }
        (incoming_hs, outgoing_hs)
    };

    if dbg {
        mesh.add_debug_vertex(v, DebugMark::new("v", egui::Color32::RED));
        mesh.add_debug_vertex(v_l, DebugMark::new("v_L", egui::Color32::RED));
        mesh.add_debug_vertex(v_r, DebugMark::new("v_R", egui::Color32::RED));
    }

    // Get the face
    let f_l_old = if !mesh.at_halfedge(h_v_l).is_boundary()? {
        Some(mesh.at_halfedge(h_v_l).face().end())
    } else {
        None
    };
    let f_r_old = if !mesh.at_halfedge(h_r_v).is_boundary()? {
        Some(mesh.at_halfedge(h_r_v).face().end())
    } else {
        None
    };

    // These halfedges will need to get re-routed
    let prev_h_r_v = mesh.at_halfedge(h_r_v).previous().end();
    let next_h_v_l = mesh.at_halfedge(h_v_l).next().end();

    if dbg {
        mesh.add_debug_halfedge(prev_h_r_v, DebugMark::green("prev_h_r_v"));
        mesh.add_debug_halfedge(next_h_v_l, DebugMark::green("next_h_v_l"));
    }

    // Allocate *all* the new structures
    let w = mesh.alloc_vertex(v_pos + delta, None);
    let h_v_w = mesh.alloc_halfedge(HalfEdge::default());
    let h_w_v = mesh.alloc_halfedge(HalfEdge::default());
    let h_l_w = mesh.alloc_halfedge(HalfEdge::default());
    let h_w_l = mesh.alloc_halfedge(HalfEdge::default());
    let h_r_w = mesh.alloc_halfedge(HalfEdge::default());
    let h_w_r = mesh.alloc_halfedge(HalfEdge::default());
    let f_l = mesh.alloc_face(None);
    let f_r = mesh.alloc_face(None);

    // --- Create the new connectivity data ---

    // Left face
    mesh[h_w_v].next = Some(h_v_l);
    mesh[h_v_l].next = Some(h_l_w);
    mesh[h_l_w].next = Some(h_w_v);
    mesh[h_w_v].face = Some(f_l);
    mesh[h_v_l].face = Some(f_l);
    mesh[h_l_w].face = Some(f_l);

    // Right face
    mesh[h_v_w].next = Some(h_w_r);
    mesh[h_w_r].next = Some(h_r_v);
    mesh[h_r_v].next = Some(h_v_w);
    mesh[h_v_w].face = Some(f_r);
    mesh[h_w_r].face = Some(f_r);
    mesh[h_r_v].face = Some(f_r);

    // Vertices
    mesh[h_v_w].vertex = Some(v);
    mesh[h_w_v].vertex = Some(w);
    mesh[h_l_w].vertex = Some(v_l);
    mesh[h_w_l].vertex = Some(w);
    mesh[h_r_w].vertex = Some(v_r);
    mesh[h_w_r].vertex = Some(w);

    // Face / vertex links
    mesh[f_l].halfedge = Some(h_l_w);
    mesh[f_r].halfedge = Some(h_w_r);
    mesh[w].halfedge = Some(h_w_v);

    // Twins
    mesh[h_v_w].twin = Some(h_w_v);
    mesh[h_w_v].twin = Some(h_v_w);

    mesh[h_l_w].twin = Some(h_w_l);
    mesh[h_w_l].twin = Some(h_l_w);

    mesh[h_r_w].twin = Some(h_w_r);
    mesh[h_w_r].twin = Some(h_r_w);

    // --- Readjust old connectivity data ---

    // It is likely that the halfedges for v, or the L and R faces are no longer
    // valid. In order to avoid a linear scan check, we just reassign those to
    // values that we already know valid
    mesh[h_w_l].face = f_l_old; // Could be none for boundary
    if let Some(f_l_old) = f_l_old {
        mesh[f_l_old].halfedge = Some(h_w_l);
    }
    mesh[h_r_w].face = f_r_old; // Could be none for boundary
    if let Some(f_r_old) = f_r_old {
        mesh[f_r_old].halfedge = Some(h_r_w);
    }
    mesh[v].halfedge = Some(h_v_w);

    // Adjust next pointers
    mesh[prev_h_r_v].next = Some(h_r_w);

    mesh[h_w_l].next = Some(next_h_v_l);

    mesh[h_r_w].next = Some(*outgoing_hs.get(0).unwrap_or(&h_w_l));
    if incoming_hs.len() > 0 {
        mesh[incoming_hs[incoming_hs.len() - 1]].next = Some(h_w_l);
    }

    // Adjust outgoing halfedge origins
    for out_h in outgoing_hs {
        mesh[out_h].vertex = Some(w);
    }

    Ok(w)
}

/// Removes `h_l` and its twin `h_r`, merging their respective faces together.
/// The face on the L side will be kept, and the R side removed. Both sides of
/// the edge that will be dissolved need to be on a face. Boundary halfedges are
/// not allowed
pub fn dissolve_edge(mesh: &mut HalfEdgeMesh, h_l: HalfEdgeId) -> Result<()> {
    // --- Collect handles ---
    let h_r = mesh.at_halfedge(h_l).twin().try_end()?;
    // If the face cannot be retrieved, a HalfedgeHasNoFace is returned
    let f_l = mesh.at_halfedge(h_l).face().try_end()?;
    let f_r = mesh.at_halfedge(h_r).face().try_end()?;
    let (v, w) = mesh.at_halfedge(h_l).src_dst_pair().unwrap();

    let h_l_nxt = mesh.at_halfedge(h_l).next().try_end()?;
    let h_l_prv = mesh.at_halfedge(h_l).previous().try_end()?;
    let h_r_nxt = mesh.at_halfedge(h_r).next().try_end()?;
    let h_r_prv = mesh.at_halfedge(h_r).previous().try_end()?;

    let halfedges_r = mesh.halfedge_loop(h_r);

    // --- Fix connectivity ---
    mesh[h_r_prv].next = Some(h_l_nxt);
    mesh[h_l_prv].next = Some(h_r_nxt);
    for h_r in halfedges_r {
        mesh[h_r].face = Some(f_l);
    }
    // Faces or vertices may point to the halfedge we're about to remove. In
    // that case we need to rotate them. We only do it in that case, to avoid
    // modifying the mesh more than necessary.
    if mesh[f_l].halfedge == Some(h_l) {
        mesh[f_l].halfedge = Some(h_l_prv);
    }
    if mesh[v].halfedge == Some(h_l) {
        mesh[v].halfedge = Some(h_l_nxt);
    }
    if mesh[w].halfedge == Some(h_r) {
        mesh[w].halfedge = Some(h_r_nxt);
    }

    // --- Remove elements ---
    mesh.remove_halfedge(h_l);
    mesh.remove_halfedge(h_r);
    mesh.remove_face(f_r);

    Ok(())
}

pub fn split_edge(
    mesh: &mut HalfEdgeMesh,
    h: HalfEdgeId,
    delta: Vec3,
    dbg: bool,
) -> Result<HalfEdgeId> {
    let (v, w) = mesh.at_halfedge(h).src_dst_pair()?;

    // NOTE: Next edge in edge loop is computed as next-twin-next
    #[rustfmt::skip]
    let (v_prev, w_next) = {
        let v_prev = mesh.at_vertex(v).halfedge_to(w).previous().twin().previous().vertex().try_end()?;
        let w_next = mesh.at_vertex(w).halfedge_to(v).previous().twin().previous().vertex().try_end()?;
        (v_prev, w_next)
    };

    if dbg {
        mesh.add_debug_vertex(v_prev, DebugMark::new("v_prv", egui::Color32::BLUE));
        mesh.add_debug_vertex(v, DebugMark::new("v", egui::Color32::BLUE));
        mesh.add_debug_vertex(w, DebugMark::new("w", egui::Color32::BLUE));
        mesh.add_debug_vertex(w_next, DebugMark::new("w_next", egui::Color32::BLUE));
    }

    let v_split = split_vertex(mesh, v, v_prev, w, delta, dbg)?;
    let w_split = split_vertex(mesh, w, v, w_next, delta, false)?;
    let arc_to_dissolve = mesh.at_vertex(w_split).halfedge_to(v).try_end()?;
    dissolve_edge(mesh, arc_to_dissolve)?;

    let new_edge = mesh.at_vertex(v_split).halfedge_to(w_split).try_end()?;

    Ok(new_edge)
}

/// Divides an edge, creating a vertex in between and a new pair of halfedges.
///
/// ## Id Stability
/// Let (v, w) the (src, dst) endpoints of h, and x the new vertex id. It is
/// guaranteed that on the new mesh, the halfedge "h" will remain on the second
/// half of the edge, that is, from x to w. The new edge will go from v to x.
/// Note that this is done in combination with the chamfer operation, whose
/// stability depends on this behavior.
pub fn divide_edge(
    mesh: &mut HalfEdgeMesh,
    h: HalfEdgeId,
    interpolation_factor: f32,
) -> Result<VertexId> {
    // Select the necessary data elements
    let h_l = h;
    let h_r = mesh.at_halfedge(h_l).twin().try_end()?;
    let h_l_prev = mesh.at_halfedge(h_l).previous().try_end()?;
    let h_r_next = mesh.at_halfedge(h_r).next().try_end()?;
    let f_l = mesh.at_halfedge(h_l).face().try_end().ok();
    let f_r = mesh.at_halfedge(h_r).face().try_end().ok();
    let (v, w) = mesh.at_halfedge(h).src_dst_pair()?;

    // Calculate the new vertex position
    let v_pos = mesh.vertex_position(v);
    let w_pos = mesh.vertex_position(w);
    let pos = v_pos.lerp(w_pos, interpolation_factor);

    // Allocate new elements
    let x = mesh.alloc_vertex(pos, None);
    let h_l_2 = mesh.alloc_halfedge(HalfEdge::default());
    let h_r_2 = mesh.alloc_halfedge(HalfEdge::default());

    // --- Update connectivity ---

    // Next pointers
    mesh[h_l_2].next = Some(h_l);
    mesh[h_l_prev].next = Some(h_l_2);
    mesh[h_r].next = Some(h_r_2);
    mesh[h_r_2].next = Some(h_r_next);

    // Twin pointers
    mesh[h_l_2].twin = Some(h_r_2);
    mesh[h_r_2].twin = Some(h_l_2);
    mesh[h_l].twin = Some(h_r);
    mesh[h_r].twin = Some(h_l);

    // Vertex pointers
    mesh[h_l].vertex = Some(x);
    mesh[h_r].vertex = Some(w);
    mesh[h_r_2].vertex = Some(x);
    mesh[h_l_2].vertex = Some(v);

    // Face pointers: May be None for boundary
    mesh[h_l_2].face = f_l;
    mesh[h_r_2].face = f_r;

    mesh[x].halfedge = Some(h_l);
    mesh[v].halfedge = Some(h_l_2);

    Ok(x)
}

pub fn cut_face(mesh: &mut halfedge::HalfEdgeMesh, v: VertexId, w: VertexId) -> Result<HalfEdgeId> {
    let face = mesh
        .at_vertex(v)
        .outgoing_halfedges()?
        .iter()
        .map(|h| mesh.at_halfedge(*h).face().try_end())
        .collect::<Result<SVec<FaceId>, TraversalError>>()?
        .iter()
        .find(|f| mesh.face_vertices(**f).contains(&w))
        .cloned()
        .ok_or(anyhow!("cut_face: v and w must share a face"))?;

    if mesh.at_vertex(v).halfedge_to(w).try_end().is_ok() {
        bail!("cut_face: v and w cannot share an edge")
    }

    let face_halfedges = mesh.face_edges(face);
    if face_halfedges.len() <= 3 {
        bail!("cut_face: cut face only works for quads or higher")
    }

    mesh.add_debug_vertex(v, DebugMark::red("v"));
    mesh.add_debug_vertex(w, DebugMark::red("w"));

    /*
    for h in mesh.at_face(face).halfedges()? {
        mesh.add_debug_halfedge(h, DebugMark::green(""));
    }
    */

    let v_idx = face_halfedges
        .iter()
        .position(|h| mesh.at_halfedge(*h).vertex().end() == v)
        .unwrap() as i32;
    let w_idx = face_halfedges
        .iter()
        .position(|h| mesh.at_halfedge(*h).vertex().end() == w)
        .unwrap() as i32;

    // NOTE: Use rem euclid so negative indices wrap up back at the end
    let h_vprev_v = face_halfedges[(v_idx - 1).rem_euclid(face_halfedges.len() as i32) as usize];
    let h_v_vnext = face_halfedges[v_idx as usize];
    let h_wprev_w = face_halfedges[(w_idx - 1).rem_euclid(face_halfedges.len() as i32) as usize];
    let h_w_wnext = face_halfedges[w_idx as usize];

    // Create new data
    let h_v_w = mesh.alloc_halfedge(HalfEdge::default());
    let h_w_v = mesh.alloc_halfedge(HalfEdge::default());
    let new_face = mesh.alloc_face(None);

    mesh[h_v_w].vertex = Some(v);
    mesh[h_w_v].vertex = Some(w);

    mesh[h_v_w].face = Some(face);
    mesh[h_w_v].face = Some(new_face);

    mesh[h_v_w].twin = Some(h_w_v);
    mesh[h_w_v].twin = Some(h_v_w);

    mesh[h_v_w].next = Some(h_w_wnext);
    mesh[h_w_v].next = Some(h_v_vnext);

    mesh[new_face].halfedge = Some(h_w_v);
    mesh[face].halfedge = Some(h_v_w);

    // Fix connectivity

    mesh[h_vprev_v].next = Some(h_v_w);
    mesh[h_wprev_w].next = Some(h_w_v);

    // The halfedges of the original face that fall on the new face
    let (start, end) = {
        let start = v_idx;
        let mut end = (w_idx - 1).rem_euclid(face_halfedges.len() as i32);
        if end < start {
            end += face_halfedges.len() as i32
        }
        (start, end)
    };
    for i in start..=end {
        let h = face_halfedges[i as usize % face_halfedges.len()];
        mesh[h].face = Some(new_face);
        mesh.add_debug_halfedge(h, DebugMark::blue(""));
    }

    Ok(h_v_w)
}

pub fn dissolve_vertex(mesh: &mut halfedge::HalfEdgeMesh, v: VertexId) -> Result<FaceId> {
    let outgoing = mesh.at_vertex(v).outgoing_halfedges()?;

    if outgoing.len() == 0 {
        bail!("Vertex {:?} is not in a face. Cannot dissolve", v);
    }

    let new_face = mesh.alloc_face(None);

    let mut to_delete = SmallVec::<[_; 16]>::new();

    // Fix next pointers for edges in the new face
    for &h in &outgoing {
        let tw = mesh.at_halfedge(h).twin().try_end()?;
        let w = mesh.at_halfedge(tw).vertex().try_end()?;
        let nxt = mesh.at_halfedge(h).next().try_end()?;
        let prv = mesh.at_halfedge(tw).previous().try_end()?;
        let f = mesh.at_halfedge(h).face().try_end()?;
        mesh[prv].next = Some(nxt);
        if mesh[w].halfedge == Some(tw) {
            mesh[w].halfedge = Some(nxt);
        }

        // We cannot safely remove data at this point, because it could be
        // accessed during `previous()` traversal.
        to_delete.push((tw, h, f));
    }

    // Set all halfedges to the same face
    let outer_loop = mesh.halfedge_loop(mesh.at_halfedge(outgoing[0]).next().try_end()?);
    for &h in &outer_loop {
        mesh[h].face = Some(new_face);
    }
    mesh[new_face].halfedge = Some(outer_loop[0]);

    mesh.remove_vertex(v);
    for (tw, h, f) in to_delete {
        mesh.remove_halfedge(tw);
        mesh.remove_halfedge(h);
        mesh.remove_face(f);
    }

    Ok(new_face)
}

/// Chamfers a vertex. That is, for each outgoing edge of the vertex, a new
/// vertex will be created. All the new vertices will be joined in a new face,
/// and the original vertex will get removed.
/// ## Id Stability
/// This operation guarantees that the outgoing halfedge ids are preserved.
/// Additionally, the returned vertex id vector has the newly created vertex ids
/// provided in the same order as `v`'s outgoing_halfedges
pub fn chamfer_vertex(
    mesh: &mut halfedge::HalfEdgeMesh,
    v: VertexId,
    interpolation_factor: f32,
) -> Result<(FaceId, SVec<VertexId>)> {
    let outgoing = mesh.at_vertex(v).outgoing_halfedges()?;
    let mut vertices = SVec::new();
    for &h in &outgoing {
        vertices.push(divide_edge(mesh, h, interpolation_factor)?);
    }

    for (&v, &w) in vertices.iter().circular_tuple_windows() {
        cut_face(mesh, v, w)?;
    }

    Ok((dissolve_vertex(mesh, v)?, vertices))
}

/// Creates a 2-sided face on the inside of this edge. This has no effect on the
/// resulting mesh, but it's useful as one of the building blocks of the bevel operation
pub fn duplicate_edge(mesh: &mut HalfEdgeMesh, h: HalfEdgeId) -> Result<HalfEdgeId> {
    let (v, w) = mesh.at_halfedge(h).src_dst_pair()?;

    let h_v_w = h;
    let h_w_v = mesh.at_halfedge(h).twin().try_end()?;

    let h2_v_w = mesh.alloc_halfedge(HalfEdge::default());
    let h2_w_v = mesh.alloc_halfedge(HalfEdge::default());

    let inner_face = mesh.alloc_face(Some(h2_v_w));

    // The two new halfedges make a cycle (2-sided face)
    mesh[h2_v_w].face = Some(inner_face);
    mesh[h2_w_v].face = Some(inner_face);
    mesh[h2_v_w].next = Some(h2_w_v);
    mesh[h2_w_v].next = Some(h2_v_w);

    mesh[h2_v_w].vertex = Some(v);
    mesh[h2_w_v].vertex = Some(w);

    // The twins point to the respective outer halfedges of the original edge
    mesh[h2_v_w].twin = Some(h_w_v);
    mesh[h2_w_v].twin = Some(h_v_w);
    mesh[h_w_v].twin = Some(h2_v_w);
    mesh[h_v_w].twin = Some(h2_w_v);

    Ok(h2_v_w)
}

/// Merges the src and dst vertices of `h` so that only the first one remains
/// TODO: This does not handle the case where a collapse edge operation would
/// remove a face
pub fn collapse_edge(mesh: &mut HalfEdgeMesh, h: HalfEdgeId) -> Result<VertexId> {
    let (v, w) = mesh.at_halfedge(h).src_dst_pair()?;
    let t = mesh.at_halfedge(h).twin().try_end()?;
    let h_next = mesh.at_halfedge(h).next().try_end()?;
    let h_prev = mesh.at_halfedge(h).previous().try_end()?;
    let t_next = mesh.at_halfedge(t).next().try_end()?;
    let t_prev = mesh.at_halfedge(t).previous().try_end()?;
    let w_outgoing = mesh.at_vertex(w).outgoing_halfedges()?;
    let v_next_fan = mesh.at_halfedge(h).cycle_around_fan().try_end()?;
    let f_h = mesh.at_halfedge(h).face().try_end();
    let f_t = mesh.at_halfedge(t).face().try_end();

    // --- Adjust connectivity ---
    for h_wo in w_outgoing {
        mesh[h_wo].vertex = Some(v);
    }
    mesh[t_prev].next = Some(t_next);
    mesh[h_prev].next = Some(h_next);

    // Some face may point to the halfedges we're deleting. Fix that.
    if let Ok(f_h) = f_h {
        if mesh.at_face(f_h).halfedge().try_end()? == h {
            mesh[f_h].halfedge = Some(h_next);
        }
    }
    if let Ok(f_t) = f_t {
        if mesh.at_face(f_t).halfedge().try_end()? == t {
            mesh[f_t].halfedge = Some(t_next);
        }
    }
    // The vertex we're keeping may be pointing to one of the deleted halfedges.
    if mesh.at_vertex(v).halfedge().try_end()? == h {
        mesh[v].halfedge = Some(v_next_fan);
    }

    // --- Remove data ----
    mesh.remove_halfedge(t);
    mesh.remove_halfedge(h);
    mesh.remove_vertex(w);

    Ok(v)
}

/// Adjusts the connectivity of the mesh in preparation for a bevel operation.
/// Any `halfedges` passed in will get "duplicated", and a face will be created
/// in-between, consistently adjusting the connectivity everywhere.
///
/// # Returns
/// A set of halfedges that participated in the bevel. These are the halfedges
/// that touched any of the original faces of the mesh. Thus, it is guaranteed
/// that any of their twins is touching a newly created face.
fn bevel_edges_connectivity(
    mesh: &mut HalfEdgeMesh,
    halfedges: &[HalfEdgeId],
) -> Result<BTreeSet<HalfEdgeId>> {
    let mut edges_to_bevel = BTreeSet::new();
    let mut duplicated_edges = BTreeSet::new();
    let mut vertices_to_chamfer = BTreeSet::new();

    // ---- 1. Duplicate all edges -----
    for &h in halfedges {
        // NOTE: Ignore edges for which we already handled its twin
        let not_yet_handled =
            edges_to_bevel.insert(h) && edges_to_bevel.insert(mesh[h].twin.unwrap());
        if not_yet_handled {
            let h_dup = duplicate_edge(mesh, h)?;
            duplicated_edges.insert(h_dup);
            duplicated_edges.insert(mesh.at_halfedge(h_dup).next().try_end()?);
            let (src, dst) = mesh.at_halfedge(h).src_dst_pair()?;
            vertices_to_chamfer.insert(src);
            vertices_to_chamfer.insert(dst);
        }
    }

    // ---- 2. Chamfer all vertices -----
    for v in vertices_to_chamfer {
        let outgoing_halfedges = mesh.at_vertex(v).outgoing_halfedges()?;

        // After the chamfer operation, some vertex pairs need to get collapsed
        // into a single one. This binary vector has a `true` for every vertex
        // position where that needs to happen.
        let collapse_indices = outgoing_halfedges
            .iter()
            .circular_tuple_windows()
            .map(|(h, h2)| {
                let h_b = edges_to_bevel.contains(h);
                let h2_b = edges_to_bevel.contains(h2);
                let h_d = duplicated_edges.contains(h);
                let h2_d = duplicated_edges.contains(h2);
                let h_n = !h_b && !h_d;
                let h2_n = !h2_b && !h2_d;

                let result = h_b && h2_n || h_d && h2_b || h_d && h2_n || h_n && h2_b;
                result
            })
            .collect::<SVecN<_, 16>>();

        // Here, we execute the chamfer operation. The returned indices are
        // guaranteed to be in the same order as `v`'s outgoing halfedges.
        let (_, new_verts) = chamfer_vertex(mesh, v, 0.0)?;

        let collapse_ops = new_verts
            .iter()
            .circular_tuple_windows()
            .zip(collapse_indices)
            .filter_map(|((v, w), should_collapse)| {
                if should_collapse {
                    // We want to keep w so next iterations don't produce dead
                    // vertex ids This is not entirely necessary since the
                    // translation map already ensures we will never access any
                    // dead vertices.
                    Some((*w, *v))
                } else {
                    None
                }
            })
            .collect::<SVecN<_, 16>>();

        // When collapsing vertices, we need a way to determine where those
        // original vertices ended up or we may access invalid ids
        type TranslationMap = HashMap<VertexId, VertexId>;
        let mut translation_map: TranslationMap = HashMap::new();
        /// Returns the translation of a vertex, that is, the vertex this vertex
        /// ended up being translated to.
        fn get_translated(m: &TranslationMap, v: VertexId) -> VertexId {
            let mut v = v;
            while let Some(v_tr) = m.get(&v) {
                v = *v_tr;
            }
            v
        }

        for (w, v) in collapse_ops {
            let v = get_translated(&translation_map, v);
            let w = get_translated(&translation_map, w);
            let h = mesh.at_vertex(w).halfedge_to(v).try_end()?;
            collapse_edge(mesh, h)?;
            translation_map.insert(v, w); // Take note that v is now w
        }
    }

    Ok(edges_to_bevel)
}

/// Bevels the given vertices by a given distance amount
pub fn bevel_edges(mesh: &mut HalfEdgeMesh, halfedges: &[HalfEdgeId], amount: f32) -> Result<()> {
    let beveled_edges = bevel_edges_connectivity(mesh, halfedges)?;

    // --- Adjust vertex positions ---

    // Movement of vertices in a bevel can be modelled as a set of pulls. For
    // each beveled edge in which the vertex participates, a certain "pull" will
    // be exerted in the direction of either the next, or previous edge
    // depending on their location of the halfedge (head, tail resp.). The final
    // move direction of a vertice is the sum of all its pulls.
    let mut move_ops = HashMap::<VertexId, HashSet<Vec3Ord>>::new();
    for h in beveled_edges {
        mesh.add_debug_halfedge(h, DebugMark::green("bvl"));

        let (v, w) = mesh.at_halfedge(h).src_dst_pair()?;
        let v_to = mesh.at_halfedge(h).previous().vertex().try_end()?;
        let v_to_pos = mesh.vertex_position(v_to);
        let w_to = mesh.at_halfedge(h).next().next().vertex().try_end()?;
        let w_to_pos = mesh.vertex_position(w_to);

        let vdir = move_ops.entry(v).or_insert(HashSet::new());
        vdir.insert(v_to_pos.to_ord());

        let wdir = move_ops.entry(w).or_insert(HashSet::new());
        wdir.insert(w_to_pos.to_ord());
    }

    for (v, v_pulls) in move_ops {
        let v_pos = mesh.vertex_position(v);
        for v_pull in v_pulls {
            let pull_to = v_pull.to_vec();
            let dir = (pull_to - v_pos).normalize();
            mesh.update_vertex_position(v, |pos| pos + dir * amount)
        }
    }

    Ok(())
}

/// Extrudes the given set of faces. Faces that are connected by at least one
/// edge will be connected after the extrude.
pub fn extrude_faces(mesh: &mut HalfEdgeMesh, faces: &[FaceId], amount: f32) -> Result<()> {
    let face_set: HashSet<FaceId> = faces.iter().cloned().collect();

    // Find the set of all halfedges not adjacent to another extruded face.
    let mut halfedges = vec![];
    for f in faces {
        for h in mesh.at_face(*f).halfedges()? {
            let twin = mesh.at_halfedge(h).twin().try_end()?;
            if let Some(tw_face) = mesh.at_halfedge(twin).face().try_end().ok() {
                if !face_set.contains(&tw_face) {
                    halfedges.push(h);
                }
            }
        }
    }

    let beveled_edges = bevel_edges_connectivity(mesh, &halfedges)?;

    // --- Adjust vertex positions ---

    // For each face, each vertex is pushed in the direction of the face's
    // normal vector. Vertices that share more than one face, get accumulated
    // pushes.
    let mut move_ops = HashMap::<VertexId, HashSet<Vec3Ord>>::new();
    for h in beveled_edges {
        // Find the halfedges adjacent to one of the extruded faces
        if mesh
            .at_halfedge(h)
            .face_or_boundary()?
            .map(|f| face_set.contains(&f))
            .unwrap_or(false)
        {
            let face = mesh.at_halfedge(h).face().try_end()?;
            let (src, dst) = mesh.at_halfedge(h).src_dst_pair()?;

            mesh.add_debug_halfedge(h, DebugMark::green("bvl"));

            let push = mesh.face_normal(face) * amount;

            move_ops
                .entry(src)
                .or_insert(HashSet::new())
                .insert(push.to_ord());
            move_ops
                .entry(dst)
                .or_insert(HashSet::new())
                .insert(push.to_ord());
        }
    }

    for (v_id, ops) in move_ops {
        mesh.update_vertex_position(v_id, |old_pos| {
            old_pos + ops.iter().fold(Vec3::ZERO, |x, y| x + y.to_vec())
        });
    }

    Ok(())
}
