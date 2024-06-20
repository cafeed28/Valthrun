use std::ffi::CStr;

use anyhow::Context;
use cs2_schema_generated::cs2::client::{
    C_PlantedC4,
    C_C4,
};
use obfstr::obfstr;
use utils_state::{
    State,
    StateCacheType,
    StateRegistry,
};

use crate::{
    CEntityIdentityEx,
    ClassNameCache,
    EntitySystem,
    Globals,
};

#[derive(Debug)]
pub struct BombDefuser {
    /// Total time remaining for a successfull bomb defuse
    pub time_remaining: f32,

    /// The defusers player name
    pub player_name: String,
}

#[derive(Debug)]
pub enum C4State {
    Active {
        /// Time remaining (in seconds) until detonation
        time_detonation: f32,
    },
    Detonated,
    Defused,
    Dropped,
    Carried,
}

/// Information about the C4
pub struct C4 {
    /// Current state of C4
    pub state: Option<C4State>,

    /// Current position of C4
    pub bomb_pos: Option<nalgebra::Vector3<f32>>,

    /// Planted bomb site
    /// 0 = A
    /// 1 = B
    pub bomb_site: Option<u8>,

    /// Current bomb defuser
    pub defuser: Option<BombDefuser>,

    // Current bomb carrier (owner)
    pub owner: Option<u32>,
}

impl State for C4 {
    type Parameter = ();

    fn create(states: &StateRegistry, _param: Self::Parameter) -> anyhow::Result<Self> {
        let globals = states.resolve::<Globals>(())?;
        let entities = states.resolve::<EntitySystem>(())?;
        let class_name_cache = states.resolve::<ClassNameCache>(())?;

        for entity_identity in entities.all_identities().iter() {
            let entity_class =
                match class_name_cache.lookup(&entity_identity.entity_class_info()?)? {
                    Some(entity_class) => entity_class,
                    None => {
                        log::warn!(
                            "Failed to get entity class info {:X}",
                            entity_identity.memory.address,
                        );
                        continue;
                    }
                };

            match entity_class.as_str() {
                "C_C4" => {
                    let bomb = entity_identity
                        .entity_ptr::<C_C4>()?
                        .read_schema()
                        .context("bomb scheme")?;

                    let game_screen_node = bomb.m_pGameSceneNode()?.read_schema()?;

                    let bomb_pos = nalgebra::Vector3::<f32>::from_column_slice(
                        &game_screen_node.m_vecAbsOrigin()?,
                    );

                    let owner = bomb.m_hOwnerEntity()?.get_entity_index();
                    if owner == 32767 {
                        return Ok(Self {
                            state: Some(C4State::Dropped),
                            bomb_pos: Some(bomb_pos),
                            bomb_site: None,
                            defuser: None,
                            owner: None,
                        });
                    } else {
                        return Ok(Self {
                            state: Some(C4State::Carried),
                            bomb_pos: Some(bomb_pos),
                            bomb_site: None,
                            defuser: None,
                            owner: Some(owner),
                        });
                    }
                }
                "C_PlantedC4" => {
                    let bomb = entity_identity
                        .entity_ptr::<C_PlantedC4>()?
                        .read_schema()
                        .context("bomb scheme")?;

                    if !bomb.m_bC4Activated()? {
                        /* This bomb hasn't been activated (yet) */
                        continue;
                    }

                    let game_screen_node = bomb.m_pGameSceneNode()?.read_schema()?;

                    let bomb_pos = nalgebra::Vector3::<f32>::from_column_slice(
                        &game_screen_node.m_vecAbsOrigin()?,
                    );

                    let bomb_site = bomb.m_nBombSite()? as u8;
                    if bomb.m_bBombDefused()? {
                        return Ok(Self {
                            state: Some(C4State::Defused),
                            bomb_pos: Some(bomb_pos),
                            bomb_site: Some(bomb_site),
                            defuser: None,
                            owner: None,
                        });
                    }

                    let time_blow = bomb.m_flC4Blow()?.m_Value()?;

                    if time_blow <= globals.time_2()? {
                        return Ok(Self {
                            state: Some(C4State::Detonated),
                            bomb_pos: Some(bomb_pos),
                            bomb_site: Some(bomb_site),
                            defuser: None,
                            owner: None,
                        });
                    }

                    let defuser = if bomb.m_bBeingDefused()? {
                        let time_defuse = bomb.m_flDefuseCountDown()?.m_Value()?;

                        let handle_defuser = bomb.m_hBombDefuser()?;
                        let defuser = entities
                            .get_by_handle(&handle_defuser)?
                            .with_context(|| {
                                obfstr!("missing bomb defuser player pawn").to_string()
                            })?
                            .entity()?
                            .reference_schema()?;

                        let defuser_controller = defuser.m_hController()?;
                        let defuser_controller = entities
                            .get_by_handle(&defuser_controller)?
                            .with_context(|| {
                                obfstr!("missing bomb defuser controller").to_string()
                            })?
                            .entity()?
                            .reference_schema()?;

                        let defuser_name =
                            CStr::from_bytes_until_nul(&defuser_controller.m_iszPlayerName()?)
                                .ok()
                                .map(CStr::to_string_lossy)
                                .unwrap_or("Name Error".into())
                                .to_string();

                        Some(BombDefuser {
                            time_remaining: time_defuse - globals.time_2()?,
                            player_name: defuser_name,
                        })
                    } else {
                        None
                    };

                    return Ok(Self {
                        state: Some(C4State::Active {
                            time_detonation: time_blow - globals.time_2()?,
                        }),
                        bomb_pos: Some(bomb_pos),
                        bomb_site: Some(bomb_site),
                        defuser,
                        owner: None,
                    });
                }
                _ => {}
            }
        }

        return Ok(Self {
            state: None,
            bomb_pos: None,
            bomb_site: None,
            defuser: None,
            owner: None,
        });
    }

    fn cache_type() -> StateCacheType {
        StateCacheType::Volatile
    }
}
