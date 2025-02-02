use self::{graph_node_ui::*, node_finder::NodeFinder};
use crate::prelude::*;
use editor_state::EditorState;
use egui::*;

use super::graph_types::{AnyParameterId, DataType};

pub mod editor_state;

pub mod graph_node_ui;

pub mod node_finder;

pub mod serialization;

/// Returns true if dirty
pub fn draw_app(ctx: &CtxRef, state: &mut EditorState) -> bool {
    let screen_rect = ctx.available_rect();
    let screen_size = ctx.available_rect().size();
    egui::TopBottomPanel::bottom("graph_panel").show(ctx, |ui| {
        let panel_height = screen_size.y * 0.5 - 4.0;
        ui.set_min_height(panel_height);
        ui.label("Graph editor");
        let clip_rect = {
            let mut r = screen_rect;
            r.set_top(panel_height);
            r
        };
        draw_graph_editor(ctx, state, clip_rect);
    });

    // When set, will load a new editor state at the end of this function
    let mut loaded_state: Option<EditorState> = None;

    egui::TopBottomPanel::top("top_menu").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            egui::menu::menu(ui, "File", |ui| {
                if ui.button("Save As...").clicked() {
                    let file_location = rfd::FileDialog::new()
                        .set_file_name("Untitled.blj")
                        .add_filter("Blackjack Models", &["blj"])
                        .save_file();
                    if let Some(path) = file_location {
                        // TODO: Do not panic for this. Show error modal instead.
                        serialization::save(state, ctx, path).expect("Serialization error");
                    }
                }
                if ui.button("Load").clicked() {
                    let file_location = rfd::FileDialog::new()
                        .add_filter("Blackjack Models", &["blj"])
                        .pick_file();
                    // TODO: Avoid panic
                    if let Some(path) = file_location {
                        loaded_state =
                            Some(serialization::load(ctx, path).expect("Deserialization error"));
                    }
                }
            });
        })
    });

    if let Some(new_state) = loaded_state {
        *state = new_state
    }

    if let Some(path) = state.load_op.take() {
        // TODO: Duplicate code
        *state = serialization::load(ctx, path.into()).expect("Deserialization error");
    }

    // TODO: Return the actual dirty flag and use it.
    true
}

pub fn draw_graph_editor(ctx: &CtxRef, state: &mut EditorState, clip_rect: Rect) {
    let mouse = &ctx.input().pointer;
    let cursor_pos = mouse.hover_pos().unwrap_or(Pos2::ZERO);

    // Gets filled with the port locations as nodes are drawn
    let mut port_locations = PortLocations::new();

    // The responses returned from node drawing have side effects that are best
    // executed at the end of this function.
    let mut delayed_responses: Vec<DrawGraphNodeResponse> = vec![];

    /* Draw nodes */
    let nodes = state.graph.iter_nodes().collect::<Vec<_>>();
    for node_id in nodes {
        let mut node_area = Area::new(node_id);
        if let Some(pos) = state.node_position_ops.remove(&node_id) {
            node_area = node_area.current_pos(pos);
        }

        node_area.show(ctx, |ui| {
            ui.set_clip_rect(clip_rect);
            let response = show_graph_node(
                &mut state.graph,
                node_id,
                ui,
                &mut port_locations,
                state.connection_in_progress,
                state
                    .active_node
                    .map(|active| active == node_id)
                    .unwrap_or(false),
            );

            if let Some(response) = response {
                delayed_responses.push(response);
            }
        });
    }

    /* Draw the node finder, if open */
    let mut should_close_node_finder = false;
    if let Some(ref mut node_finder) = state.node_finder {
        let mut node_finder_area = Area::new("node_finder");
        if let Some(pos) = node_finder.position {
            node_finder_area = node_finder_area.current_pos(pos);
        }
        node_finder_area.show(ctx, |ui| {
            if let Some(node_archetype) = node_finder.show(ui) {
                let new_node = state.graph.add_node(node_archetype.to_descriptor());
                state.node_position_ops.insert(new_node, cursor_pos);
                should_close_node_finder = true;
            }
        });
    }
    if should_close_node_finder {
        state.node_finder = None;
    }

    /* Draw connections */
    let connection_stroke = egui::Stroke {
        width: 5.0,
        color: color_from_hex("#efefef").unwrap(),
    };

    if let Some((_, ref locator)) = state.connection_in_progress {
        let painter = ctx.layer_painter(LayerId::background());
        let start_pos = port_locations[&locator];
        painter.line_segment([start_pos, cursor_pos], connection_stroke)
    }

    for (input, output) in state.graph.iter_connections() {
        let painter = ctx.layer_painter(LayerId::background());
        let src_pos = port_locations[&AnyParameterId::Output(output)];
        let dst_pos = port_locations[&AnyParameterId::Input(input)];
        painter.line_segment([src_pos, dst_pos], connection_stroke);
    }

    /* Handle responses from drawing nodes */

    for response in delayed_responses {
        match response {
            DrawGraphNodeResponse::ConnectEventStarted(node_id, port) => {
                state.connection_in_progress = Some((node_id, port));
            }
            DrawGraphNodeResponse::ConnectEventEnded(locator) => {
                let in_out = match (
                    state
                        .connection_in_progress
                        .map(|(_node, param)| param)
                        .take()
                        .expect("Cannot end drag without in-progress connection."),
                    locator,
                ) {
                    (AnyParameterId::Input(input), AnyParameterId::Output(output))
                    | (AnyParameterId::Output(output), AnyParameterId::Input(input)) => {
                        Some((input, output))
                    }
                    _ => None,
                };

                if let Some((input, output)) = in_out {
                    state.graph.add_connection(output, input)
                }
            }
            DrawGraphNodeResponse::SetActiveNode(node_id) => {
                state.active_node = Some(node_id);
            }
            DrawGraphNodeResponse::ClearActiveNode => {
                state.active_node = None;
            }
            DrawGraphNodeResponse::RunNodeSideEffect(node_id) => {
                state.run_side_effect = Some(node_id);
            }
            DrawGraphNodeResponse::DeleteNode(node_id) => {
                state.graph.remove_node(node_id);
                // Make sure to not leave references to old nodes hanging
                if state.active_node.map(|x| x == node_id).unwrap_or(false) {
                    state.active_node = None;
                }
                if state.run_side_effect.map(|x| x == node_id).unwrap_or(false) {
                    state.run_side_effect = None;
                }
            }
            DrawGraphNodeResponse::DisconnectEvent(input_id) => {
                let corresp_output = state
                    .graph
                    .connection(input_id)
                    .expect("Connection data should be valid");
                let other_node = state.graph.get_input(input_id).node();
                state.graph.remove_connection(input_id);
                state.connection_in_progress =
                    Some((other_node, AnyParameterId::Output(corresp_output)));
            }
        }
    }

    /* Mouse input handling */

    if mouse.any_released() && state.connection_in_progress.is_some() {
        state.connection_in_progress = None;
    }

    if mouse.button_down(PointerButton::Secondary) {
        state.node_finder = Some(NodeFinder::new_at(cursor_pos));
    }
    if ctx.input().key_pressed(Key::Escape) {
        state.node_finder = None;
    }
}
