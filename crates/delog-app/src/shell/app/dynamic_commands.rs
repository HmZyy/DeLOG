#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicFamily {
    Parser,
    Script,
    Layout,
    LiveLink,
}

pub const fn dynamic_command_families() -> &'static [DynamicFamily] {
    &[
        DynamicFamily::Parser,
        DynamicFamily::Script,
        DynamicFamily::Layout,
        DynamicFamily::LiveLink,
    ]
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DynamicCommandNames {
    pub layouts: Vec<String>,
    pub scripts: Vec<String>,
    pub parsers: Vec<String>,
}

pub fn merge_dynamic_command_refresh(
    previous: &DynamicCommandNames,
    layouts: Option<Vec<String>>,
    scripts: Option<Vec<String>>,
    parsers: Option<Vec<String>>,
) -> DynamicCommandNames {
    DynamicCommandNames {
        layouts: layouts.unwrap_or_else(|| previous.layouts.clone()),
        scripts: scripts.unwrap_or_else(|| previous.scripts.clone()),
        parsers: parsers.unwrap_or_else(|| previous.parsers.clone()),
    }
}

pub fn merge_fallible_dynamic_command_refresh<LayoutError, ScriptError, ParserError>(
    previous: &DynamicCommandNames,
    layouts: Result<Vec<String>, LayoutError>,
    scripts: Result<Vec<String>, ScriptError>,
    parsers: Result<Vec<String>, ParserError>,
) -> DynamicCommandNames {
    merge_dynamic_command_refresh(previous, layouts.ok(), scripts.ok(), parsers.ok())
}

#[derive(Debug, Default)]
pub struct DynamicCommandCatalog {
    names: DynamicCommandNames,
    initialized: bool,
    dirty: bool,
}

impl DynamicCommandCatalog {
    pub fn ensure_with<E>(&mut self, loader: impl FnOnce() -> Result<DynamicCommandNames, E>) {
        if self.initialized && !self.dirty {
            return;
        }
        self.initialized = true;
        self.dirty = false;
        if let Ok(names) = loader() {
            self.names = names;
        }
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn names(&self) -> &DynamicCommandNames {
        &self.names
    }
}
