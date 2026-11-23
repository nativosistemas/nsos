//! Capa de arquitectura.
//!
//! Cada arquitectura ofrece la misma interfaz al resto del kernel:
//!
//! * `TrapFrame` con `kernel_thread`, `user_thread`, `is_user_mode`,
//!   `syscall_nr`/`syscall_args`/`set_syscall_ret` y `redirect_to_kernel`,
//!   e `initial_frame` (dónde va el frame de un hilo nuevo);
//! * `cpu`: interrupciones (`enable_interrupts`, `without_interrupts`...) y
//!   `wait_for_interrupt`;
//! * `paging`: `AddressSpace` (con entradas neutras, ver
//!   `nsos_kcore::paging`), `switch_to`, `install`, `harden` y
//!   `protections`;
//! * `time`: timer del scheduler y reloj;
//! * `discover` (lee el device tree, antes de `mm::init`), `init_devices`;
//! * `serial`, `init_cpu`, `yield_now`, `breakpoint`, `set_kernel_stack`,
//!   `FAULT_PAGE`, `FAULT_EXEC`, `configure`, `halt_forever` y `qemu_exit`.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
