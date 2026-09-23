use std::cell::Cell;

use delog_api::timestamps::TimestampMode;

thread_local! {
    static CURRENT_SCRIPT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static TIMESTAMP_MODE: Cell<TimestampMode> = const { Cell::new(TimestampMode::Effective) };
}

pub(crate) fn set_current_script(name: Option<String>) {
    CURRENT_SCRIPT.with(|current| *current.borrow_mut() = name);
}

pub(crate) fn current_script() -> Option<String> {
    CURRENT_SCRIPT.with(|current| current.borrow().clone())
}

pub(crate) fn with_timestamp_mode<T>(mode: TimestampMode, f: impl FnOnce() -> T) -> T {
    TIMESTAMP_MODE.with(|current| {
        struct Reset<'a> {
            current: &'a Cell<TimestampMode>,
            previous: TimestampMode,
        }

        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.current.set(self.previous);
            }
        }

        let reset = Reset {
            current,
            previous: current.replace(mode),
        };
        let result = f();
        drop(reset);
        result
    })
}

pub(crate) fn current_timestamp_mode() -> TimestampMode {
    TIMESTAMP_MODE.get()
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

    #[test]
    fn timestamp_mode_resets_after_the_scoped_call() {
        assert_eq!(current_timestamp_mode(), TimestampMode::Effective);
        with_timestamp_mode(TimestampMode::Original, || {
            assert_eq!(current_timestamp_mode(), TimestampMode::Original);
        });
        assert_eq!(current_timestamp_mode(), TimestampMode::Effective);
    }
}
