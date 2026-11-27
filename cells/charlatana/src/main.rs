//! Cell `charlatana`: prueba negativa de la cuota de memoria del kernel.
//!
//! Arranca sin capacidades y con una cuota chica (`CHARLATANA_QUOTA` en
//! `kernel/src/main.rs`). Crea canales y se manda mensajes a sí
//! misma (con los datos llenos) sin recibirlos nunca, así cada uno queda en una cola del kernel.
//! Cuando una cola se llena (`WouldBlock`) crea otro canal; sigue hasta que
//! el kernel le dice `NoMemory`. Termina con la cantidad de mensajes que
//! dejó encolados: ktest comprueba que caben en su cuota. Antes de las
//! cuotas de memoria del kernel, llenaba 512 canales (el tope de su CSpace)
//! con 64 mensajes cada uno, unos 10 MiB del heap del kernel; con varias
//! cells así, el kernel se quedaba sin heap.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::MAX_DATA;
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{Error, Message};
use nsos_rt::{channel_create, channel_send, println};

nsos_rt::entry!(main);

fn main() -> i32 {
    let Ok(msg) = Message::new(1, &[0x55; MAX_DATA]) else {
        return -1;
    };
    let mut queued = 0;
    'canales: loop {
        let (a, _b) = match channel_create() {
            Ok(p) => p,
            Err(Error::NoMemory) => break,
            Err(e) => {
                println!("charlatana: error inesperado al crear un canal: {:?}", e);
                return -1;
            }
        };
        loop {
            match channel_send(a, &msg, NONBLOCK) {
                Ok(()) => queued += 1,
                Err(Error::WouldBlock) => continue 'canales,
                Err(Error::NoMemory) => break 'canales,
                Err(e) => {
                    println!("charlatana: error inesperado al enviar: {:?}", e);
                    return -1;
                }
            }
        }
    }
    println!(
        "charlatana: {} mensajes encolados antes de NoMemory",
        queued
    );
    queued
}
