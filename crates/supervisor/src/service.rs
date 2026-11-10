//! The life of a long-running service, without any operating system in it.
//!
//! [`lifecycle`] models running, a stop or preshutdown request, the end of
//! admission, a drain to a monotonic deadline, termination and the
//! quiescence check a service passes before it says it has stopped.
//! It calls no operating system, so it is tested the same way everywhere;
//! the Windows Service Control Manager binding only feeds it requests and
//! carries out what it reports.
pub mod lifecycle;
