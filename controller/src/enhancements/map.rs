use std::path::PathBuf;

use cs2::EntitySystem;
use cs2_schema_generated::cs2::client::CSkeletonInstance;
use imgui::ImColor32;

use super::Enhancement;
use crate::{
    physics::{
        Ray,
        RayHit,
        WorldPhysics,
    },
    view::ViewController,
};

pub struct MapVis {
    physics: WorldPhysics,

    local_position: nalgebra::Vector3<f32>,
    ray_hit: Option<RayHit>,
}

impl MapVis {
    pub fn new() -> anyhow::Result<Self> {
        let physics = WorldPhysics::load(PathBuf::from("C:\\Program Files (x86)\\Steam\\steamapps\\common\\Counter-Strike Global Offensive\\game\\csgo\\maps\\de_mirage.vpk"))?;

        Ok(Self {
            physics,

            local_position: Default::default(),
            ray_hit: None,
        })
    }
}

impl Enhancement for MapVis {
    fn update(&mut self, ctx: &crate::UpdateContext) -> anyhow::Result<()> {
        let entities = ctx.states.resolve::<EntitySystem>(())?;

        let lpc = entities.get_local_player_controller()?;
        if lpc.is_null()? {
            return Ok(());
        }

        let lpc = lpc.reference_schema()?;
        let player_pawn = match { entities.get_by_handle(&lpc.m_hPlayerPawn()?)? } {
            Some(pawn) => pawn.entity()?.read_schema()?,
            None => return Ok(()),
        };

        let game_screen_node = player_pawn
            .m_pGameSceneNode()?
            .cast::<CSkeletonInstance>()
            .read_schema()?;

        let mut v_angle = player_pawn.v_angle()?;
        v_angle[0] = -v_angle[0];
        v_angle[0] = v_angle[0].to_radians();
        v_angle[1] = v_angle[1].to_radians();

        self.local_position =
            nalgebra::Vector3::<f32>::from_column_slice(&game_screen_node.m_vecAbsOrigin()?);

        let direction = nalgebra::Vector3::new(
            v_angle[1].cos() * v_angle[0].cos(),
            v_angle[1].sin() * v_angle[0].cos(),
            v_angle[0].sin(),
        );
        let ray = Ray {
            origin: self.local_position.clone() + nalgebra::Vector3::<f32>::new(0.0, 0.0, 64.0),
            direction,
            max_distance: f32::MAX,
        };

        self.ray_hit = self.physics.trace(&ray);
        Ok(())
    }

    fn render(&self, states: &utils_state::StateRegistry, ui: &imgui::Ui) -> anyhow::Result<()> {
        let view = states.resolve::<ViewController>(())?;

        let draw = ui.get_window_draw_list();
        let ray_target = match &self.ray_hit {
            Some(target) => target,
            None => return Ok(()),
        };

        if let Some(pos) = view.world_to_screen(&ray_target.location, true) {
            draw.add_circle(pos, 5.0, ImColor32::from_rgb(0xFF, 0x0, 0xFF))
                .filled(true)
                .build();

            draw.add_text(
                [pos.x, pos.y + 20.0],
                ImColor32::from_rgb(0xFF, 0x0, 0xFF),
                &format!("{:.2}", (self.local_position - ray_target.location).norm()),
            );
        }

        Ok(())
    }

    fn render_debug_window(&mut self, _states: &utils_state::StateRegistry, _ui: &imgui::Ui) {}
}
