//! Códigos de error de las syscalls.
//!
//! Una syscall devuelve en `rax` un `i64`: `>= 0` es éxito (valor útil o 0) y
//! `< 0` es `-(código de error)`.

/// Error devuelto por el kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Error {
    /// Número de syscall desconocido.
    NoSys = 1,
    /// El handle no existe en el CSpace de la cell o es de una generación vieja.
    BadHandle = 2,
    /// El objeto referido no es del tipo que pide la syscall.
    WrongType = 3,
    /// La capacidad no tiene los derechos necesarios.
    AccessDenied = 4,
    /// Argumento inválido (puntero fuera de rango, longitud, flags...).
    InvalidArgs = 5,
    /// La operación bloquearía y se pidió `NONBLOCK`.
    WouldBlock = 6,
    /// El otro extremo del canal fue cerrado (o su cell murió).
    PeerClosed = 7,
    /// Sin memoria o sin slots libres en el CSpace.
    NoMemory = 8,
    /// Se agotó el tiempo de espera.
    TimedOut = 9,
    /// El mensaje no cumple las reglas de formato.
    BadMessage = 10,
    /// La operación no está soportada todavía por este kernel.
    Unsupported = 11,
}

/// Resultado de una syscall ya decodificado.
pub type SysResult<T> = Result<T, Error>;

impl Error {
    /// Todos los errores, útil para tests y tablas.
    pub const ALL: [Error; 11] = [
        Error::NoSys,
        Error::BadHandle,
        Error::WrongType,
        Error::AccessDenied,
        Error::InvalidArgs,
        Error::WouldBlock,
        Error::PeerClosed,
        Error::NoMemory,
        Error::TimedOut,
        Error::BadMessage,
        Error::Unsupported,
    ];

    /// Convierte un código numérico en `Error`.
    pub const fn from_code(code: u16) -> Option<Error> {
        Some(match code {
            1 => Error::NoSys,
            2 => Error::BadHandle,
            3 => Error::WrongType,
            4 => Error::AccessDenied,
            5 => Error::InvalidArgs,
            6 => Error::WouldBlock,
            7 => Error::PeerClosed,
            8 => Error::NoMemory,
            9 => Error::TimedOut,
            10 => Error::BadMessage,
            11 => Error::Unsupported,
            _ => return None,
        })
    }

    /// Codifica el error como valor de retorno de syscall (negativo).
    pub const fn to_ret(self) -> i64 {
        -(self as u16 as i64)
    }
}

/// Codifica un resultado como valor de `rax`.
pub fn encode_ret(r: SysResult<u64>) -> i64 {
    match r {
        // Los valores de éxito deben caber en un i64 positivo.
        Ok(v) if v <= i64::MAX as u64 => v as i64,
        Ok(_) => Error::InvalidArgs.to_ret(),
        Err(e) => e.to_ret(),
    }
}

/// Decodifica el valor de `rax` devuelto por una syscall.
pub fn decode_ret(raw: i64) -> SysResult<u64> {
    if raw >= 0 {
        Ok(raw as u64)
    } else {
        let code = raw.unsigned_abs();
        match u16::try_from(code).ok().and_then(Error::from_code) {
            Some(e) => Err(e),
            None => Err(Error::InvalidArgs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_errors() {
        for e in Error::ALL {
            assert_eq!(decode_ret(encode_ret(Err(e))), Err(e));
            assert_eq!(Error::from_code(e as u16), Some(e));
        }
    }

    #[test]
    fn success_values() {
        assert_eq!(decode_ret(encode_ret(Ok(0))), Ok(0));
        assert_eq!(decode_ret(encode_ret(Ok(42))), Ok(42));
        assert_eq!(encode_ret(Ok(u64::MAX)), Error::InvalidArgs.to_ret());
    }

    #[test]
    fn unknown_negative_codes_are_rejected() {
        assert_eq!(decode_ret(-999), Err(Error::InvalidArgs));
        assert_eq!(decode_ret(i64::MIN), Err(Error::InvalidArgs));
    }
}
