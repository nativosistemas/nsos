//! Bindings mínimos del protocolo de arranque Limine.
//!
//! Se escriben a mano (en vez de usar la crate `limine`) para compilar en Rust
//! estable y mantener el `unsafe` acotado a este archivo. Solo se declaran las
//! requests que el kernel usa. Referencia: PROTOCOL.md de Limine.
//!
//! # Invariantes (`unsafe`)
//! - Limine escribe las respuestas **antes** de saltar a `kmain` y nunca
//!   después; el kernel es monohilo en ese momento. Por eso leer los punteros
//!   de respuesta con `read_volatile` y convertirlos a `&'static` es correcto.
//! - Las respuestas viven en memoria `BOOTLOADER_RECLAIMABLE`. Hasta que el
//!   allocador físico la reclame (hito 3) siguen siendo válidas.

use core::cell::UnsafeCell;
use core::ffi::{c_char, CStr};

const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

/// Revisión base del protocolo que pedimos.
pub const REQUESTED_BASE_REVISION: u64 = 3;

/// Marca de revisión base. Limine pone a 0 el tercer elemento si la soporta.
#[repr(C)]
pub struct BaseRevision(UnsafeCell<[u64; 3]>);

// SAFETY: solo Limine escribe (antes de kmain); luego solo hay lecturas.
unsafe impl Sync for BaseRevision {}

impl BaseRevision {
    const fn new(rev: u64) -> Self {
        Self(UnsafeCell::new([
            0xf9562b2d5c95a6c8,
            0x6a7b384944536bdc,
            rev,
        ]))
    }

    /// `true` si el bootloader aceptó la revisión pedida.
    pub fn is_supported(&self) -> bool {
        // SAFETY: puntero a un static válido y alineado; ver invariantes.
        unsafe { (self.0.get() as *const u64).add(2).read_volatile() == 0 }
    }
}

/// Request genérica: id de 4 palabras, revisión y puntero de respuesta.
#[repr(C)]
pub struct Request<R: 'static> {
    id: [u64; 4],
    revision: u64,
    response: UnsafeCell<*const R>,
}

// SAFETY: ídem BaseRevision.
unsafe impl<R> Sync for Request<R> {}

impl<R> Request<R> {
    const fn new(id: [u64; 2]) -> Self {
        Self {
            id: [COMMON_MAGIC[0], COMMON_MAGIC[1], id[0], id[1]],
            revision: 0,
            response: UnsafeCell::new(core::ptr::null()),
        }
    }

    /// Respuesta del bootloader, si la hubo.
    pub fn response(&self) -> Option<&'static R> {
        // SAFETY: ver invariantes del módulo; un puntero no nulo apunta a una
        // respuesta válida e inmutable escrita por Limine.
        unsafe {
            let p = self.response.get().read_volatile();
            p.as_ref()
        }
    }
}

/// Respuesta de información del bootloader.
#[repr(C)]
pub struct BootloaderInfo {
    revision: u64,
    name: *const c_char,
    version: *const c_char,
}

impl BootloaderInfo {
    fn cstr(p: *const c_char) -> &'static str {
        if p.is_null() {
            return "?";
        }
        // SAFETY: Limine garantiza cadenas C terminadas en NUL y estáticas.
        unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("?")
    }
    /// Nombre del bootloader.
    pub fn name(&self) -> &'static str {
        Self::cstr(self.name)
    }
    /// Versión del bootloader.
    pub fn version(&self) -> &'static str {
        Self::cstr(self.version)
    }
}

/// Respuesta con la línea de comandos del kernel (de `limine.conf`).
#[repr(C)]
pub struct Cmdline {
    revision: u64,
    cmdline: *const c_char,
}

impl Cmdline {
    /// Línea de comandos completa.
    pub fn as_str(&self) -> &'static str {
        BootloaderInfo::cstr(self.cmdline)
    }
}

/// Respuesta con la dirección física y virtual donde se cargó el kernel.
#[repr(C)]
pub struct ExecutableAddress {
    revision: u64,
    /// Base física.
    pub physical_base: u64,
    /// Base virtual.
    pub virtual_base: u64,
}

/// Archivo cargado por Limine (módulo).
#[repr(C)]
pub struct File {
    revision: u64,
    address: *const u8,
    size: u64,
    path: *const c_char,
    cmdline: *const c_char,
}

impl File {
    /// Ruta del módulo en el medio de arranque.
    pub fn path(&self) -> &'static str {
        BootloaderInfo::cstr(self.path)
    }
    /// Contenido del módulo.
    pub fn bytes(&self) -> &'static [u8] {
        if self.address.is_null() {
            return &[];
        }
        // SAFETY: Limine garantiza `size` bytes válidos en `address` (memoria
        // KernelAndModules, nunca reclamada por el kernel).
        unsafe { core::slice::from_raw_parts(self.address, self.size as usize) }
    }
}

/// Respuesta con los módulos cargados.
#[repr(C)]
pub struct Modules {
    revision: u64,
    count: u64,
    modules: *const *const File,
}

impl Modules {
    /// Itera los módulos.
    pub fn iter(&self) -> impl Iterator<Item = &'static File> + '_ {
        (0..self.count as usize).filter_map(move |i| {
            // SAFETY: Limine garantiza `count` punteros válidos.
            unsafe { (*self.modules.add(i)).as_ref() }
        })
    }
}

/// Respuesta HHDM: desplazamiento del mapeo directo de la memoria física.
#[repr(C)]
pub struct Hhdm {
    revision: u64,
    /// Dirección virtual = física + `offset`.
    pub offset: u64,
}

/// Tipos de región del mapa de memoria.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemKind {
    Usable,
    Reserved,
    AcpiReclaimable,
    AcpiNvs,
    BadMemory,
    BootloaderReclaimable,
    KernelAndModules,
    Framebuffer,
    Other(u64),
}

impl MemKind {
    fn from_raw(v: u64) -> MemKind {
        match v {
            0 => MemKind::Usable,
            1 => MemKind::Reserved,
            2 => MemKind::AcpiReclaimable,
            3 => MemKind::AcpiNvs,
            4 => MemKind::BadMemory,
            5 => MemKind::BootloaderReclaimable,
            6 => MemKind::KernelAndModules,
            7 => MemKind::Framebuffer,
            o => MemKind::Other(o),
        }
    }
}

/// Entrada del mapa de memoria.
#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    kind: u64,
}

impl MemmapEntry {
    pub fn kind(&self) -> MemKind {
        MemKind::from_raw(self.kind)
    }
}

/// Respuesta del mapa de memoria.
#[repr(C)]
pub struct Memmap {
    revision: u64,
    entry_count: u64,
    entries: *const *const MemmapEntry,
}

impl Memmap {
    /// Itera las entradas (ordenadas por base, sin solaparse, según el protocolo).
    pub fn entries(&self) -> impl Iterator<Item = &'static MemmapEntry> + '_ {
        (0..self.entry_count as usize).filter_map(move |i| {
            // SAFETY: Limine garantiza `entry_count` punteros válidos.
            unsafe { (*self.entries.add(i)).as_ref() }
        })
    }
}

#[used]
#[link_section = ".requests_start_marker"]
static START_MARKER: [u64; 4] = [
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
];

#[used]
#[link_section = ".requests"]
pub static BASE_REVISION: BaseRevision = BaseRevision::new(REQUESTED_BASE_REVISION);

#[used]
#[link_section = ".requests"]
pub static BOOTLOADER_INFO: Request<BootloaderInfo> =
    Request::new([0xf55038d8e2a1202f, 0x279426fcf5f59740]);

#[used]
#[link_section = ".requests"]
pub static CMDLINE: Request<Cmdline> = Request::new([0x4b161536e598651e, 0xb390ad4a2f1f303a]);

#[used]
#[link_section = ".requests"]
pub static EXECUTABLE_ADDRESS: Request<ExecutableAddress> =
    Request::new([0x71ba76863cc55f63, 0xb2644a48c516a487]);

#[used]
#[link_section = ".requests"]
pub static MODULES: Request<Modules> = Request::new([0x3e7e279702be32af, 0xca1c4f3bd1280cee]);

#[used]
#[link_section = ".requests"]
pub static HHDM: Request<Hhdm> = Request::new([0x48dcf1cb8ad2b852, 0x63984e959a98244b]);

#[used]
#[link_section = ".requests"]
pub static MEMMAP: Request<Memmap> = Request::new([0x67cf3d9d378a806f, 0xe304acdfc50c3c62]);

#[used]
#[link_section = ".requests_end_marker"]
static END_MARKER: [u64; 2] = [0xadc0e0531bb10d03, 0x9572709f31764c62];
