use anyhow::Context;
use cs2::{
    C4State,
    CEntityIdentityEx,
    EntitySystem,
    C4,
};
use cs2_schema_generated::cs2::client::C_CSPlayerPawn;

use super::Enhancement;
use crate::{
    settings::AppSettings,
    utils::ImguiUiEx,
    view::ViewController,
};
pub struct BombInfoIndicator {
    local_pos: Option<nalgebra::Vector3<f32>>,
}
use obfstr::obfstr;

impl BombInfoIndicator {
    pub fn new() -> Self {
        Self {
            local_pos: Default::default(),
        }
    }
}

/// % of the screens height
const PLAYER_AVATAR_TOP_OFFSET: f32 = 0.004;

/// % of the screens height
const PLAYER_AVATAR_SIZE: f32 = 0.05;

const UNITS_TO_METERS: f32 = 0.01905;

// Max distance that bomb causes dmg in meters
// TODO: radius for each map
const IS_SAFE: f32 = 1768.0 * UNITS_TO_METERS;

impl Enhancement for BombInfoIndicator {
    fn update(&mut self, ctx: &crate::UpdateContext) -> anyhow::Result<()> {
        let entities = ctx.states.resolve::<EntitySystem>(())?;

        let local_player_controller = entities
            .get_local_player_controller()?
            .try_reference_schema()
            .with_context(|| obfstr!("failed to read local player controller").to_string())?;

        let player_controller = match local_player_controller {
            Some(controller) => controller,
            None => {
                /* We're currently not connected */
                return Ok(());
            }
        };

        let observice_entity_handle = if player_controller.m_bPawnIsAlive()? {
            player_controller.m_hPawn()?.get_entity_index()
        } else {
            let local_obs_pawn =
                match { entities.get_by_handle(&player_controller.m_hObserverPawn()?)? } {
                    Some(pawn) => pawn.entity()?.reference_schema()?,
                    None => {
                        /* this is odd... */
                        return Ok(());
                    }
                };

            local_obs_pawn
                .m_pObserverServices()?
                .read_schema()?
                .m_hObserverTarget()?
                .get_entity_index()
        };

        for entity_identity in entities.all_identities() {
            if entity_identity.handle::<()>()?.get_entity_index() == observice_entity_handle {
                /* current pawn we control/observe */
                let local_pawn = entity_identity
                    .entity_ptr::<C_CSPlayerPawn>()?
                    .read_schema()?;
                let local_pos =
                    nalgebra::Vector3::<f32>::from_column_slice(&local_pawn.m_vOldOrigin()?);
                self.local_pos = Some(local_pos);
                continue;
            }
        }
        Ok(())
    }

    fn render(&self, states: &utils_state::StateRegistry, ui: &imgui::Ui) -> anyhow::Result<()> {
        let settings = states.resolve::<AppSettings>(())?;
        if !settings.bomb_esp {
            return Ok(());
        }

        let bomb_state = states.resolve::<C4>(())?;
        let view = states.resolve::<ViewController>(())?;

        if let Some(state) = &bomb_state.state {
            let group = ui.begin_group();
            let line_count = match state {
                C4State::Active { .. } => 3,
                C4State::Defused => 2,
                _ => 0,
            };
            let text_height = ui.text_line_height_with_spacing() * line_count as f32;

            /* align to be on the right side after the players */
            let offset_x = ui.io().display_size[0] * 1730.0 / 2560.0;
            let offset_y = ui.io().display_size[1] * PLAYER_AVATAR_TOP_OFFSET
                + 0_f32.max((ui.io().display_size[1] * PLAYER_AVATAR_SIZE - text_height) / 2.0);

            if settings.bomb_esp_settings.bomb_site
                && matches!(state, C4State::Active { time_detonation: _ })
            {
                ui.set_cursor_pos([offset_x, offset_y]);
                ui.text(&format!(
                    "Bomb planted {}",
                    match bomb_state.bomb_site.unwrap_or(0) {
                        0 => "A",
                        1 => "B",
                        _ => unreachable!(),
                    }
                ));
            }

            if settings.bomb_esp_settings.bomb_status {
                ui.set_cursor_pos_x(offset_x);
                match state {
                    C4State::Active { time_detonation } => {
                        ui.text(&format!("Time: {:.3}", time_detonation));

                        if let Some(defuser) = &bomb_state.defuser {
                            let color = if defuser.time_remaining > *time_detonation {
                                [0.79, 0.11, 0.11, 1.0]
                            } else {
                                [0.11, 0.79, 0.26, 1.0]
                            };
                            ui.set_cursor_pos_x(offset_x);
                            ui.text_colored(
                                color,
                                &format!(
                                    "Defused in {:.3} by {}",
                                    defuser.time_remaining, defuser.player_name
                                ),
                            );
                        } else {
                            ui.set_cursor_pos_x(offset_x);
                            ui.text("Not defusing");
                        }
                    }
                    C4State::Defused => {
                        ui.text("Bomb has been defused");
                    }
                    _ => {}
                }
            }

            if matches!(state, C4State::Dropped)
                || matches!(state, C4State::Active { time_detonation: _ })
            {
                if let Some(pos) = bomb_state.bomb_pos {
                    if let Some(local_pos) = self.local_pos {
                        let time_detonation = match state {
                            C4State::Active { time_detonation } => Some(*time_detonation),
                            _ => None,
                        };

                        let distance = (pos - local_pos).norm() * UNITS_TO_METERS;
                        let color = if settings.bomb_esp_settings.bomb_position {
                            settings
                                .bomb_esp_settings
                                .bomb_position_color
                                .calculate_color(distance, time_detonation.unwrap_or(40.0))
                        } else {
                            [1.0, 1.0, 1.0, 1.0]
                        };

                        if settings.bomb_esp_settings.bomb_position {
                            if let Some(pos) = view.world_to_screen(&pos, false) {
                                let y_offset = 0.0;
                                let draw = ui.get_window_draw_list();
                                let text = "BOMB";
                                let [text_width, _] = ui.calc_text_size(&text);
                                let mut pos = pos.clone();
                                pos.x -= text_width / 2.0;
                                pos.y += y_offset;
                                ui.set_cursor_pos_x(offset_x);
                                draw.add_text(pos, color, text);
                            }
                        }

                        if settings.bomb_esp_settings.is_safe && time_detonation.is_some() {
                            ui.set_cursor_pos_x(offset_x);
                            if distance > IS_SAFE {
                                ui.text("You're safe!")
                            } else {
                                let test = IS_SAFE - distance;
                                let text = format!("Back {:.0} m", test);
                                ui.text(text)
                            };
                        }
                    }
                }
            }
            group.end();
        }

        Ok(())
    }
}
