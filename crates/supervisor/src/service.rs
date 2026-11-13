//! The life of a long-running service, without any operating system in it.
//!
//! [`lifecycle`] models running, a stop or preshutdown request, the end of
//! admission, a drain to a monotonic deadline, termination and the
//! quiescence check a service passes before it says it has stopped.
//! [`drain`] carries the launches a service holds through that ending,
//! [`control`] is what the control handler does with a control code,
//! [`startup`] judges what an earlier run journaled, and [`pipe_policy`]
//! decides which client a service's named pipe may serve. [`settings`] holds
//! the service's configuration, and [`quiet`] keeps a failure that repeats
//! from filling the service's log.
//!
//! None of it calls an operating system, so it is tested the same way
//! everywhere. The Windows Service Control Manager binding, the named pipe
//! and the runner service (`host_runtime::windows`) only feed it facts and
//! carry out what it decides.
pub mod control;
pub mod drain;
pub mod lifecycle;
pub mod pipe_policy;
pub mod quiet;
pub mod settings;
pub mod stand_in;
pub mod startup;
pub mod supervisor;
