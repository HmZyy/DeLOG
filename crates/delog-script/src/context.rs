thread_local! {
    static CURRENT_SCRIPT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn set_current_script(name: Option<String>) {
    CURRENT_SCRIPT.with(|current| *current.borrow_mut() = name);
}

pub(crate) fn current_script() -> Option<String> {
    CURRENT_SCRIPT.with(|current| current.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_script_thread_local_roundtrips() {
        assert_eq!(current_script(), None);
        set_current_script(Some("s".into()));
        assert_eq!(current_script(), Some("s".into()));
        set_current_script(None);
        assert_eq!(current_script(), None);
    }
}
