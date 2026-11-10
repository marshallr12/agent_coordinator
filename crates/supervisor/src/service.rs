//! The life of a long-running service, without any operating system in it.
//!
//! [`lifecycle`] models running, a stop or preshutdown request, the end of
//! admission, a drain to a monotonic deadline, termination and the
//! quiescence check a service passes before it says it has stopped.
//! [`drain`] carries the launches a service holds through that ending.
//! [`pipe_policy`] decides which client a service's named pipe may serve.
//! None of it calls an operating system, so it is tested the same way everywhere;
//! the Windows Service Control Manager binding only feeds it requests and
//! carries out what it reports.
pub mod drain;
pub mod lifecycle;
pub mod pipe_policy;
pub mod settings;
