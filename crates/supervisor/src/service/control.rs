//! What a service's control handler does with a control code: record it and
//! answer. The handler runs on a thread of the operating system's and must
//! return at once, so it never stops a launch, waits, allocates or takes a
//! lock; the worker does that work after it sees the recorded request.
use super::lifecycle::{Request, Signal};

/// `SERVICE_CONTROL_STOP`.
pub const STOP: u32 = 1;
/// `SERVICE_CONTROL_INTERROGATE`.
pub const INTERROGATE: u32 = 4;
/// `SERVICE_CONTROL_PRESHUTDOWN`.
pub const PRESHUTDOWN: u32 = 15;
/// `NO_ERROR`: the control was handled.
pub const HANDLED: u32 = 0;
/// `ERROR_CALL_NOT_IMPLEMENTED`: the service does not take this control.
pub const NOT_IMPLEMENTED: u32 = 120;

/// What the handler decided about one control code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handled {
    /// The value the handler returns to the operating system.
    pub answer: u32,
    /// Whether the worker has to be woken to see a new request.
    pub wake: bool,
}

/// Records `control` in `signal` if it ends the service and says how to
/// answer. An interrogation needs no work here: the worker's last status
/// report is the answer.
pub fn on_control(signal: &Signal, control: u32) -> Handled {
    let request = match control {
        STOP => Some(Request::Stop),
        PRESHUTDOWN => Some(Request::Preshutdown),
        INTERROGATE => {
            return Handled {
                answer: HANDLED,
                wake: false,
            };
        }
        _ => {
            return Handled {
                answer: NOT_IMPLEMENTED,
                wake: false,
            };
        }
    };
    if let Some(request) = request {
        signal.request(request);
    }
    Handled {
        answer: HANDLED,
        wake: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_and_preshutdown_are_recorded_and_answered_without_the_work() {
        let signal = Signal::default();
        let handled = on_control(&signal, STOP);
        assert_eq!(
            handled,
            Handled {
                answer: HANDLED,
                wake: true
            }
        );
        assert!(signal.stop_requested() && !signal.preshutdown_requested());
        let handled = on_control(&signal, PRESHUTDOWN);
        assert_eq!(
            handled,
            Handled {
                answer: HANDLED,
                wake: true
            }
        );
        assert!(signal.preshutdown_requested());
    }

    #[test]
    fn an_interrogation_records_nothing_and_every_other_control_is_not_implemented() {
        let signal = Signal::default();
        let handled = on_control(&signal, INTERROGATE);
        assert_eq!(
            handled,
            Handled {
                answer: HANDLED,
                wake: false
            }
        );
        // PAUSE, CONTINUE, SHUTDOWN, PARAMCHANGE, a user-defined code.
        for control in [2, 3, 5, 6, 128] {
            let handled = on_control(&signal, control);
            assert_eq!(
                handled,
                Handled {
                    answer: NOT_IMPLEMENTED,
                    wake: false
                }
            );
        }
        assert!(!signal.stop_requested() && !signal.preshutdown_requested());
    }
}
