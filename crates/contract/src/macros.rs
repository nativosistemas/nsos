//! La macro `contract!`.

/// Define un contrato de canal.
///
/// ```
/// use nsos_contract::{contract, Bytes};
///
/// contract! {
///     /// Eco: el cliente manda texto y el servidor lo devuelve.
///     pub contract echo {
///         messages {
///             /// Pedido de eco.
///             Echo(1) { /** Texto. */ data: Bytes<200> },
///             /// Respuesta.
///             EchoReply(2) { /** El mismo texto. */ data: Bytes<200> },
///             /// Fin.
///             Bye(999) {},
///         }
///         start Ready;
///         client Ready { Echo => Waiting, Bye => End }
///         server Waiting { EchoReply => Ready }
///         end End;
///     }
/// }
/// # fn main() {}
/// ```
///
/// * `messages`: cada mensaje con su etiqueta y sus campos (tipos que
///   implementan [`Field`](crate::Field)).
/// * `client S { M => D, ... }`: en el estado `S` manda el cliente; mandar
///   `M` lleva a `D`. `server` es lo mismo del otro lado, y `end` declara
///   un estado final. Van en ese orden (primero todos los `client`, después
///   los `server`, al final los `end`), que es también la numeración de los
///   estados en la tabla.
///
/// La macro se usa a nivel de módulo (no dentro de una función): el
/// módulo que genera importa lo del módulo que la contiene con
/// `use super::*`.
///
/// Genera el módulo `echo` con un struct por mensaje, `TRANSITIONS`,
/// `START`, `BOUND` (la cota de mensajes en vuelo), `pair` (crea un canal
/// con un extremo de cada lado) y los módulos `client` y `server` con un
/// tipo por estado. En cada lado, `recv::S` es el `enum` de lo que puede
/// llegar en el estado `S`.
///
/// Al compilar comprueba con [`check`](crate::check) que el contrato esté
/// bien formado (ningún estado deja mandar a las dos puntas, cada etiqueta
/// lleva a un solo estado, la cota entra en la cola del kernel), que las
/// etiquetas no se repitan y que cada mensaje entre en un [`Message`]
/// (`crate::abi::Message`). Un mensaje repetido en un estado es un error de
/// compilación (variante repetida).
#[macro_export]
macro_rules! contract {
    (
        $(#[$meta:meta])*
        $vis:vis contract $name:ident {
            messages {
                $(
                    $(#[$mmeta:meta])*
                    $msg:ident($label:expr) {
                        $( $(#[$fmeta:meta])* $field:ident : $fty:ty ),* $(,)?
                    }
                ),* $(,)?
            }
            start $start:ident;
            $( client $cs:ident { $( $cmsg:ident => $cdst:ident ),* $(,)? } )*
            $( server $ss:ident { $( $smsg:ident => $sdst:ident ),* $(,)? } )*
            $( end $es:ident; )*
        }
    ) => {
        $(#[$meta])*
        #[allow(unused_imports)]
        $vis mod $name {
            use super::*;

            $(
                $(#[$mmeta])*
                #[derive(Debug)]
                pub struct $msg {
                    $( $(#[$fmeta])* pub $field: $fty, )*
                }

                impl $crate::Msg for $msg {
                    const LABEL: u64 = $label;

                    fn encode(self, m: &mut $crate::abi::Message) {
                        #[allow(unused_mut, unused_variables)]
                        let mut w = $crate::Writer::new(m, $label);
                        let $msg { $( $field, )* } = self;
                        $( $crate::Field::put($field, &mut w); )*
                    }

                    fn decode<T: $crate::Transport>(
                        t: T,
                        m: &$crate::abi::Message,
                    ) -> ::core::option::Option<Self> {
                        if m.header.label != $label {
                            return ::core::option::Option::None;
                        }
                        #[allow(unused_mut)]
                        let mut r = $crate::Reader::new(m);
                        $( let $field = <$fty as $crate::Field>::take(&mut r, t)?; )*
                        let _ = t;
                        if !r.finish() {
                            return ::core::option::Option::None;
                        }
                        ::core::option::Option::Some($msg { $( $field, )* })
                    }
                }

                const _: () = assert!(
                    $crate::__fits(
                        0 $( + <$fty as $crate::Field>::DATA )*,
                        0 $( + <$fty as $crate::Field>::CAPS )*
                    ),
                    "contrato: un mensaje no entra en los datos o capacidades de un Message"
                );
            )*

            #[allow(dead_code)]
            #[derive(Clone, Copy)]
            #[repr(u16)]
            enum StateId { $( $cs, )* $( $ss, )* $( $es, )* }

            /// Tabla del contrato (la que comprueba `check` y compara el
            /// test `contract_model` con el modelo en Lean).
            pub const TRANSITIONS: &[$crate::Transition] = &[
                $( $( $crate::Transition {
                    src: StateId::$cs as u16,
                    dir: $crate::Dir::ClientToServer,
                    label: <$cmsg as $crate::Msg>::LABEL,
                    dst: StateId::$cdst as u16,
                }, )* )*
                $( $( $crate::Transition {
                    src: StateId::$ss as u16,
                    dir: $crate::Dir::ServerToClient,
                    label: <$smsg as $crate::Msg>::LABEL,
                    dst: StateId::$sdst as u16,
                }, )* )*
            ];

            /// Estado inicial.
            pub const START: u16 = StateId::$start as u16;

            /// Etiquetas de todos los mensajes.
            pub const LABELS: &[u64] = &[ $( $label, )* ];

            /// Máximo de mensajes en vuelo entre las dos puntas
            /// (`Contract.queue_bound`).
            pub const BOUND: u32 = $crate::__assert_contract(TRANSITIONS, LABELS);
            const _: u32 = BOUND;

            /// Crea un canal con un extremo de cada lado, en el estado inicial.
            pub fn pair<T: $crate::Transport>(
                t: T,
            ) -> ::core::result::Result<
                ($crate::Chan<T, client::$start>, $crate::Chan<T, server::$start>),
                $crate::abi::Error,
            > {
                let (a, b) = $crate::Transport::channel_create(t)?;
                ::core::result::Result::Ok((
                    $crate::Chan::__from_parts(t, a),
                    $crate::Chan::__from_parts(t, b),
                ))
            }

            $crate::__contract_side! {
                client, "el cliente",
                sends { $( $cs { $( $cmsg => $cdst ),* } )* }
                receives { $( $ss { $( $smsg => $sdst ),* } )* }
                ends { $( $es )* }
            }
            $crate::__contract_side! {
                server, "el servidor",
                sends { $( $ss { $( $smsg => $sdst ),* } )* }
                receives { $( $cs { $( $cmsg => $cdst ),* } )* }
                ends { $( $es )* }
            }
        }
    };
}

/// Un lado de un contrato: los estados en que manda, en los que recibe y
/// los finales.
#[doc(hidden)]
#[macro_export]
macro_rules! __contract_side {
    (
        $side:ident, $who:literal,
        sends { $( $s:ident { $( $smsg:ident => $sdst:ident ),* } )* }
        receives { $( $r:ident { $( $rmsg:ident => $rdst:ident ),* } )* }
        ends { $( $e:ident )* }
    ) => {
        #[doc = concat!("Lado de ", $who, ": un tipo por estado.")]
        pub mod $side {
            use super::*;

            $(
                #[doc = concat!("Estado `", stringify!($s), "`: manda ", $who, ".")]
                #[derive(Debug)]
                pub struct $s;
                impl $crate::State for $s {
                    const ID: u16 = StateId::$s as u16;
                }
                $( impl $crate::Sends<super::$smsg> for $s {
                    type Next = $sdst;
                } )*
            )*

            $(
                #[doc = concat!("Estado `", stringify!($r), "`: recibe ", $who, ".")]
                #[derive(Debug)]
                pub struct $r;
                impl $crate::State for $r {
                    const ID: u16 = StateId::$r as u16;
                }
                impl $crate::Receives for $r {
                    type Branch<T: $crate::Transport> = recv::$r<T>;

                    fn decode<T: $crate::Transport>(
                        t: T,
                        h: $crate::abi::CapHandle,
                        m: &$crate::abi::Message,
                    ) -> ::core::option::Option<recv::$r<T>> {
                        $(
                            if m.header.label == <super::$rmsg as $crate::Msg>::LABEL {
                                let msg = <super::$rmsg as $crate::Msg>::decode(t, m)?;
                                return ::core::option::Option::Some(recv::$r::$rmsg(
                                    $crate::Chan::__from_parts(t, h),
                                    msg,
                                ));
                            }
                        )*
                        let _ = (t, h, m);
                        ::core::option::Option::None
                    }
                }
            )*

            $(
                #[doc = concat!("Estado final `", stringify!($e), "`.")]
                #[derive(Debug)]
                pub struct $e;
                impl $crate::State for $e {
                    const ID: u16 = StateId::$e as u16;
                }
            )*

            /// Lo que puede llegar en cada estado en que se recibe.
            pub mod recv {
                $(
                    #[doc = concat!("Lo que puede llegar en `", stringify!($r), "`.")]
                    // Los mensajes van por valor: las cells no tienen heap.
                    #[allow(clippy::large_enum_variant)]
                    pub enum $r<T: $crate::Transport> {
                        $(
                            #[doc = concat!("`", stringify!($rmsg), "`; el canal sigue en `", stringify!($rdst), "`.")]
                            $rmsg($crate::Chan<T, super::$rdst>, super::super::$rmsg),
                        )*
                    }

                    impl<T: $crate::Transport> ::core::fmt::Debug for $r<T> {
                        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                            match *self {
                                $( $r::$rmsg(ref c, ref m) => f
                                    .debug_tuple(stringify!($rmsg))
                                    .field(c)
                                    .field(m)
                                    .finish(), )*
                            }
                        }
                    }
                )*
            }
        }
    };
}
