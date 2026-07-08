use egui_dock::{DockState, TabPath, TabViewer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppDockTab {
    Diagnostics,
    Performance,
    Markers,
    #[cfg(feature = "scripting")]
    ScriptingConsole,
    Logging,
}

const FIXED_ORDER: &[AppDockTab] = &[
    AppDockTab::Diagnostics,
    AppDockTab::Performance,
    AppDockTab::Markers,
    #[cfg(feature = "scripting")]
    AppDockTab::ScriptingConsole,
    AppDockTab::Logging,
];

#[derive(Debug)]
pub struct AppDockController {
    state: DockState<AppDockTab>,
    active_tab: Option<AppDockTab>,
}

impl AppDockController {
    pub fn new_empty() -> Self {
        Self {
            state: DockState::new(Vec::new()),
            active_tab: None,
        }
    }

    pub fn open_or_focus(&mut self, tab: AppDockTab) {
        if !self.is_open(tab) {
            let mut tabs = self.open_tabs();
            tabs.push(tab);
            self.replace_tabs(tabs);
        }
        self.focus_tab(tab);
    }

    pub fn close(&mut self, tab: AppDockTab) {
        let active_closed = self.active_tab == Some(tab);
        let tabs = self
            .open_tabs()
            .into_iter()
            .filter(|candidate| *candidate != tab)
            .collect();
        self.replace_tabs(tabs);
        self.active_tab = if active_closed {
            self.first_tab()
        } else {
            self.active_tab
                .filter(|active| self.is_open(*active))
                .or_else(|| self.first_tab())
        };
        if let Some(active) = self.active_tab {
            self.focus_tab(active);
        }
    }

    pub fn is_open(&self, tab: AppDockTab) -> bool {
        self.state.find_tab(&tab).is_some()
    }

    pub fn has_tabs(&self) -> bool {
        self.tab_count() > 0
    }

    pub fn tab_count(&self) -> usize {
        self.state
            .iter_surfaces_indexed()
            .map(|(_, surface)| surface.iter_all_tabs().count())
            .sum()
    }

    pub fn open_tabs(&self) -> Vec<AppDockTab> {
        FIXED_ORDER
            .iter()
            .copied()
            .filter(|tab| self.is_open(*tab))
            .collect()
    }

    #[cfg(test)]
    fn active_tab(&self) -> Option<AppDockTab> {
        self.active_tab.filter(|tab| self.is_open(*tab))
    }

    pub fn show_inside(
        &mut self,
        ui: &mut egui::Ui,
        viewer: &mut impl TabViewer<Tab = AppDockTab>,
    ) {
        egui_dock::DockArea::new(&mut self.state)
            .id(egui::Id::new("app_dock_area"))
            .style(egui_dock::Style::from_egui(ui.style().as_ref()))
            .allowed_splits(egui_dock::AllowedSplits::None)
            .draggable_tabs(false)
            .tab_context_menus(false)
            .show_close_buttons(true)
            .show_leaf_close_all_buttons(true)
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, viewer);
        self.reconcile_active_tab();
    }

    fn replace_tabs(&mut self, tabs: Vec<AppDockTab>) {
        self.state = DockState::new(ordered_tabs(tabs));
    }

    fn focus_tab(&mut self, tab: AppDockTab) {
        if let Some(path) = self.state.find_tab(&tab) {
            self.focus_path(path, tab);
        }
    }

    fn focus_path(&mut self, path: TabPath, tab: AppDockTab) {
        if self.state.set_active_tab(path).is_ok() {
            self.state.set_focused_node_and_surface(path.node_path());
            self.active_tab = Some(tab);
        } else {
            self.active_tab = self.first_tab();
        }
    }

    fn reconcile_active_tab(&mut self) {
        self.active_tab = self
            .state
            .find_active_focused()
            .map(|(_, tab)| *tab)
            .or_else(|| self.first_tab());
    }

    fn first_tab(&self) -> Option<AppDockTab> {
        self.open_tabs().into_iter().next()
    }
}

fn ordered_tabs(tabs: Vec<AppDockTab>) -> Vec<AppDockTab> {
    FIXED_ORDER
        .iter()
        .copied()
        .filter(|tab| tabs.contains(tab))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        let docks = AppDockController::new_empty();
        assert!(!docks.has_tabs());
        assert_eq!(docks.active_tab(), None);
    }

    #[test]
    fn open_or_focus_adds_tab_and_selects_it() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Diagnostics);
        assert!(docks.has_tabs());
        assert!(docks.is_open(AppDockTab::Diagnostics));
        assert_eq!(docks.active_tab(), Some(AppDockTab::Diagnostics));
    }

    #[test]
    fn repeated_open_or_focus_does_not_duplicate_tab() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Logging);
        docks.open_or_focus(AppDockTab::Logging);
        assert_eq!(docks.tab_count(), 1);
        assert_eq!(docks.active_tab(), Some(AppDockTab::Logging));
    }

    #[test]
    fn opening_multiple_docks_keeps_one_fixed_order_tab_strip() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Markers);
        docks.open_or_focus(AppDockTab::Diagnostics);
        docks.open_or_focus(AppDockTab::Performance);
        assert_eq!(docks.tab_count(), 3);
        assert!(docks.is_open(AppDockTab::Diagnostics));
        assert!(docks.is_open(AppDockTab::Performance));
        assert!(docks.is_open(AppDockTab::Markers));
        assert_eq!(
            docks.open_tabs(),
            vec![
                AppDockTab::Diagnostics,
                AppDockTab::Performance,
                AppDockTab::Markers
            ]
        );
        assert_eq!(docks.active_tab(), Some(AppDockTab::Performance));
    }

    #[test]
    fn close_removes_tab_and_allows_reopen() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Diagnostics);
        docks.close(AppDockTab::Diagnostics);
        assert!(!docks.has_tabs());
        assert!(!docks.is_open(AppDockTab::Diagnostics));

        docks.open_or_focus(AppDockTab::Diagnostics);
        assert!(docks.is_open(AppDockTab::Diagnostics));
        assert_eq!(docks.active_tab(), Some(AppDockTab::Diagnostics));
    }

    #[test]
    fn focusing_existing_tab_updates_active_tab() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Diagnostics);
        docks.open_or_focus(AppDockTab::Logging);
        docks.open_or_focus(AppDockTab::Diagnostics);
        assert_eq!(docks.tab_count(), 2);
        assert_eq!(docks.active_tab(), Some(AppDockTab::Diagnostics));
    }

    #[test]
    fn reconcile_active_tab_syncs_from_state() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Diagnostics);
        docks.open_or_focus(AppDockTab::Logging);

        let logging_path = docks.state.find_tab(&AppDockTab::Logging).unwrap();
        docks.state.set_active_tab(logging_path).unwrap();
        docks
            .state
            .set_focused_node_and_surface(logging_path.node_path());
        docks.active_tab = Some(AppDockTab::Diagnostics);

        docks.reconcile_active_tab();

        assert_eq!(docks.active_tab(), Some(AppDockTab::Logging));
    }

    #[test]
    fn closing_active_tab_selects_first_remaining_fixed_order_tab() {
        let mut docks = AppDockController::new_empty();
        docks.open_or_focus(AppDockTab::Diagnostics);
        docks.open_or_focus(AppDockTab::Performance);
        docks.open_or_focus(AppDockTab::Logging);

        docks.close(AppDockTab::Logging);

        assert_eq!(
            docks.open_tabs(),
            vec![AppDockTab::Diagnostics, AppDockTab::Performance]
        );
        assert_eq!(docks.active_tab(), Some(AppDockTab::Diagnostics));
    }
}
