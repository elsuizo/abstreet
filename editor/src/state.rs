use crate::colors::ColorScheme;
use crate::objects::{Ctx, RenderingHints, ID};
use crate::plugins::debug::DebugMode;
use crate::plugins::edit::EditMode;
use crate::plugins::logs::DisplayLogs;
use crate::plugins::sim::SimMode;
use crate::plugins::time_travel::TimeTravel;
use crate::plugins::view::ViewMode;
use crate::plugins::{Plugin, PluginCtx};
use crate::render::Renderable;
use crate::ui::PerMapUI;
use abstutil::Timer;
use ezgui::{Canvas, Color, GfxCtx, UserInput};
use map_model::IntersectionID;
use sim::{GetDrawAgents, SimFlags};

pub trait UIState {
    fn handle_zoom(&mut self, old_zoom: f64, new_zoom: f64);
    fn set_current_selection(&mut self, obj: Option<ID>);
    fn event(
        &mut self,
        input: &mut UserInput,
        hints: &mut RenderingHints,
        recalculate_current_selection: &mut bool,
        cs: &mut ColorScheme,
        canvas: &mut Canvas,
    );
    fn get_objects_onscreen(
        &self,
        canvas: &Canvas,
    ) -> (Vec<Box<&Renderable>>, Vec<Box<Renderable>>);
    fn is_debug_mode_enabled(&self) -> bool;
    fn draw(&self, g: &mut GfxCtx, ctx: &Ctx);
    fn dump_before_abort(&self);
    fn color_obj(&self, id: ID, ctx: &Ctx) -> Option<Color>;
    fn primary(&self) -> &PerMapUI;
}

pub struct DefaultUIState {
    pub primary: PerMapUI,
    primary_plugins: PluginsPerMap,
    // When running an A/B test, this is populated too.
    secondary: Option<(PerMapUI, PluginsPerMap)>,
    plugins: PluginsPerUI,
    active_plugin: Option<usize>,
}

impl DefaultUIState {
    pub fn new(flags: SimFlags, kml: Option<String>, canvas: &Canvas) -> DefaultUIState {
        // Do this first to trigger the log console initialization, so anything logged by sim::load
        // isn't lost.
        let plugins = PluginsPerUI::new();
        let (primary, primary_plugins) = PerMapUI::new(flags, kml, &canvas);
        DefaultUIState {
            primary,
            primary_plugins,
            secondary: None,
            plugins,
            active_plugin: None,
        }
    }

    fn get_active_plugin(&self) -> Option<&Plugin> {
        let idx = self.active_plugin?;
        match idx {
            x if x == 0 => Some(&self.plugins.edit_mode),
            x if x == 1 => Some(&self.plugins.sim_mode),
            x if x == 2 => Some(&self.plugins.logs),
            x if x == 3 => Some(&self.primary_plugins.debug_mode),
            x if x == 4 => Some(&self.primary_plugins.view_mode),
            x if x == 5 => Some(&self.primary_plugins.time_travel),
            _ => {
                panic!("Illegal active_plugin {}", idx);
            }
        }
    }

    fn run_plugin(
        &mut self,
        idx: usize,
        input: &mut UserInput,
        hints: &mut RenderingHints,
        recalculate_current_selection: &mut bool,
        cs: &mut ColorScheme,
        canvas: &mut Canvas,
    ) -> bool {
        let mut ctx = PluginCtx {
            primary: &mut self.primary,
            primary_plugins: None,
            secondary: &mut self.secondary,
            canvas,
            cs,
            input,
            hints,
            recalculate_current_selection,
        };
        match idx {
            x if x == 0 => {
                ctx.primary_plugins = Some(&mut self.primary_plugins);
                self.plugins.edit_mode.blocking_event(&mut ctx)
            }
            x if x == 1 => {
                ctx.primary_plugins = Some(&mut self.primary_plugins);
                self.plugins.sim_mode.blocking_event(&mut ctx)
            }
            x if x == 2 => self.plugins.logs.blocking_event(&mut ctx),
            x if x == 3 => self.primary_plugins.debug_mode.blocking_event(&mut ctx),
            x if x == 4 => self.primary_plugins.view_mode.blocking_event(&mut ctx),
            x if x == 5 => self.primary_plugins.time_travel.blocking_event(&mut ctx),
            _ => {
                panic!("Illegal active_plugin {}", idx);
            }
        }
    }
}

impl UIState for DefaultUIState {
    fn handle_zoom(&mut self, old_zoom: f64, new_zoom: f64) {
        self.primary_plugins
            .debug_mode
            .layers
            .handle_zoom(old_zoom, new_zoom);
    }

    fn set_current_selection(&mut self, obj: Option<ID>) {
        self.primary.current_selection = obj;
    }

    fn event(
        &mut self,
        input: &mut UserInput,
        hints: &mut RenderingHints,
        recalculate_current_selection: &mut bool,
        cs: &mut ColorScheme,
        canvas: &mut Canvas,
    ) {
        // If there's an active plugin, just run it.
        if let Some(idx) = self.active_plugin {
            if !self.run_plugin(idx, input, hints, recalculate_current_selection, cs, canvas) {
                self.active_plugin = None;
            }
        } else {
            // Run each plugin, short-circuiting if the plugin claimed it was active.
            for idx in 0..=5 {
                if self.run_plugin(idx, input, hints, recalculate_current_selection, cs, canvas) {
                    self.active_plugin = Some(idx);
                    break;
                }
            }
        }
    }

    fn get_objects_onscreen(
        &self,
        canvas: &Canvas,
    ) -> (Vec<Box<&Renderable>>, Vec<Box<Renderable>>) {
        let draw_agent_source: &GetDrawAgents = {
            let tt = &self.primary_plugins.time_travel;
            if tt.is_active() {
                tt
            } else {
                &self.primary.sim
            }
        };

        self.primary.draw_map.get_objects_onscreen(
            canvas.get_screen_bounds(),
            &self.primary_plugins.debug_mode,
            &self.primary.map,
            draw_agent_source,
            self,
        )
    }

    fn is_debug_mode_enabled(&self) -> bool {
        self.primary_plugins
            .debug_mode
            .layers
            .debug_mode
            .is_enabled()
    }

    fn draw(&self, g: &mut GfxCtx, ctx: &Ctx) {
        if let Some(p) = self.get_active_plugin() {
            p.draw(g, ctx);
        } else {
            // If no other mode was active, give the ambient plugins in ViewMode and SimMode a
            // chance.
            self.primary_plugins.view_mode.draw(g, ctx);
            self.plugins.sim_mode.draw(g, ctx);
        }
    }

    fn dump_before_abort(&self) {
        error!("********************************************************************************");
        error!("UI broke! Primary sim:");
        self.primary.sim.dump_before_abort();
        if let Some((s, _)) = &self.secondary {
            error!("Secondary sim:");
            s.sim.dump_before_abort();
        }
    }

    fn color_obj(&self, id: ID, ctx: &Ctx) -> Option<Color> {
        if Some(id) == self.primary.current_selection {
            return Some(ctx.cs.get_def("selected", Color::BLUE));
        }

        if let Some(p) = self.get_active_plugin() {
            p.color_for(id, ctx)
        } else {
            // If no other mode was active, give the ambient plugins in ViewMode a chance.
            self.primary_plugins.view_mode.color_for(id, ctx)
        }
    }

    fn primary(&self) -> &PerMapUI {
        &self.primary
    }
}

pub trait ShowTurnIcons {
    fn show_icons_for(&self, id: IntersectionID) -> bool;
}

impl ShowTurnIcons for DefaultUIState {
    fn show_icons_for(&self, id: IntersectionID) -> bool {
        self.primary_plugins
            .debug_mode
            .layers
            .show_all_turn_icons
            .is_enabled()
            || self.plugins.edit_mode.show_turn_icons(id)
            || {
                if let Some(ID::Turn(t)) = self.primary.current_selection {
                    t.parent == id
                } else {
                    false
                }
            }
    }
}

// aka plugins that don't depend on map
pub struct PluginsPerUI {
    edit_mode: EditMode,
    sim_mode: SimMode,
    logs: DisplayLogs,
}

impl PluginsPerUI {
    pub fn new() -> PluginsPerUI {
        PluginsPerUI {
            edit_mode: EditMode::new(),
            sim_mode: SimMode::new(),
            logs: DisplayLogs::new(),
        }
    }
}

pub struct PluginsPerMap {
    // Anything that holds onto any kind of ID has to live here!
    debug_mode: DebugMode,
    view_mode: ViewMode,
    time_travel: TimeTravel,
}

impl PluginsPerMap {
    pub fn new(state: &PerMapUI, canvas: &Canvas, timer: &mut Timer) -> PluginsPerMap {
        let mut plugins = PluginsPerMap {
            debug_mode: DebugMode::new(&state.map),
            view_mode: ViewMode::new(&state.map, &state.draw_map, timer),
            time_travel: TimeTravel::new(),
        };
        plugins.debug_mode.layers.handle_zoom(-1.0, canvas.cam_zoom);
        plugins
    }
}