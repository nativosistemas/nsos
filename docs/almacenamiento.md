# Almacenamiento propio

nsos guarda datos en un disco con su propio formato: **objetos** con dueño,
nombre y versión, escritos con **copy-on-write** para que una caída nunca
deje un objeto a medias. El acceso a cada objeto es una **capacidad**.

```
kernel: pci::find(1af4:1001) -> BAR0 + anillos DMA + búfer de datos
                                                    (un Memory, dos capacidades)
+---------+  canal: INFO/READ/WRITE/FLUSH  +---------+  raíz dueño 1  +----------+
| virtblk | <----------------------------> | almacen | <------------> | escritor |
+---------+   búfer compartido (DMA)       +---------+  raíz dueño 2  +----------+
     |                                         ^      <------------> | curioso  |
     v                                         |                     +----------+
disco virtio (QEMU) -> archivo              handles: un extremo de canal por objeto abierto
```

## Piezas

| Pieza | Qué hace |
|---|---|
| `cells/virtblk` | Driver virtio-blk (PCI legacy) en user-space. Protocolo **genérico de bloques** de 512 bytes: `INFO`, `READ`, `WRITE`, `FLUSH`. No sabe nada del formato. |
| `crates/store` (`nsos-store`) | Toda la lógica del almacén: formato en disco, copy-on-write, montaje y handles. Sin `unsafe`, sin syscalls: se prueba en el host. |
| `cells/almacen` | Une las dos: monta el disco a través del driver y atiende a los clientes. |
| `cells/escritor`, `cells/curioso` | Clientes de prueba (dueños 1 y 2). |

El búfer de datos es un solo objeto `Memory` con dos capacidades: la del
driver tiene `CONTROL` (necesita la dirección física para DMA) y la del
almacén no. El almacén copia ahí los bloques y el dispositivo los lee y
escribe directamente.

## Formato en disco

Bloques de 4 KiB. Los bloques 0 y 1 son **dos ranuras de superbloque**; el
resto guarda tablas y datos.

* **Superbloque**: marca `NSOSALM1`, número de **secuencia**, dónde está la
  tabla y el SHA-256 de la tabla. Lleva su propio SHA-256.
* **Tabla** (un bloque, hasta 30 objetos): por cada objeto, dueño, nombre
  (hasta 31 bytes), versión, largo, dónde están sus datos (bloques
  contiguos, hasta 64 KiB) y el SHA-256 de los datos.

El estado del disco es el de la ranura **válida** de mayor secuencia.
Válida quiere decir que el superbloque, la tabla y los datos de cada objeto
coinciden con sus hashes.

Detalle: `crates/store/src/layout.rs`.

## Cómo se escribe sin dejar nada a medias

Escribir un objeto (`Store::put`):

1. Los datos nuevos van a bloques **libres**: ninguno que use el estado
   confirmado (ni sus tablas ni sus datos).
2. La tabla nueva (con todos los objetos) va a otro bloque libre. Los demás
   objetos no se copian: la tabla nueva apunta a sus mismos bloques.
3. `FLUSH`.
4. El superbloque nuevo, con secuencia + 1, va a la **otra** ranura.
5. `FLUSH`. Recién ahora el objeto nuevo existe.

Si se corta la luz en cualquier punto, la ranura vieja sigue válida y todo
lo que usa sigue intacto. La nueva solo gana si quedó entera. Por eso al
montar aparece el estado viejo o el nuevo, nunca una mezcla.

Al montar hay un detalle más: si la otra ranura tiene un superbloque de
secuencia igual o mayor que no valida (de una escritura que no terminó), se
borra. Si no, una escritura posterior que justo reusara esos bloques podría
completar su árbol y "resucitar" un estado que nunca se confirmó. El modelo
en Lean encontró este caso (`Store.stale_resurrects`).

Cada objeto tiene una **versión** que sube en cada escritura. `WRITE` puede
pedir una versión esperada (compare-and-swap): si otro escribió antes, se
rechaza.

## Capacidades: quién accede a qué

* El supervisor (hoy el kernel) le da a cada dueño una **raíz**: un canal
  con el almacén. El dueño de un pedido sale de la raíz por la que llegó,
  nunca del mensaje. Dos dueños pueden tener objetos con el mismo nombre:
  son objetos distintos.
* `OPEN` por la raíz devuelve un **handle**: un extremo de canal nuevo que
  da acceso a ese objeto y a nada más. Tenerlo es la autoridad; el kernel
  garantiza que no se falsifica (`Sys.no_forging`).
* `DUP_RO` da una copia de **solo lectura**, para pasársela a otra cell. Con
  ella se lee, pero escribir se rechaza, y copiarla de nuevo da otra de solo
  lectura.

Protocolo completo: `crates/store/src/proto.rs`.

## Verificación formal

Modelo en `verification/lean/Nsos/Store.lean`, escrito antes que el código:

* `Store.crash_safe`: una caída en cualquier punto de una escritura (cada
  escritura hecha, no hecha o a medias, y en cualquier orden) se recupera
  como el estado viejo o el nuevo.
* `Store.commit_good`, `Store.mount_good`, `Store.crash_then_mount`: el
  invariante vale después de escribir, de montar y de caerse, así que se
  puede seguir escribiendo.
* `Store.rw_handle_from_owner`, `Store.change_needs_owner`,
  `Store.untouched_without_owner`: un objeto de un dueño solo cambia por un
  handle escribible que abrió la raíz de ese dueño.
* Ejemplos con `decide` de por qué hace falta cada regla
  (`overwrite_breaks`, `same_slot_breaks`, `same_seq_loses`,
  `stale_resurrects`).

En el código: los tests de `crates/store` prueban **todas** las
combinaciones de escrituras hechas, no hechas o rotas (sector a sector) de
una escritura, como cuantifica `crash_safe`. Kani prueba que el
decodificador de entradas no confía en el disco y que el mapa de bloques
libres no devuelve un bloque usado.

## Cómo probarlo

En QEMU x86_64 y en QEMU `virt` aarch64 (`ARCH=aarch64 tools/store-test.sh`; ahí el disco de arranque es NVMe, así el único virtio-blk es el del almacén).

```sh
make smoke-almacen   # la prueba completa: escribir, caída simulada,
                     # 3 veces kill -9 a QEMU, y un disco FAT que no se toca
make demo-almacen    # lo mismo con 10 rondas de kill -9
```

A mano, cortando vos la luz:

```sh
make run-almacen                 # disco build/almacen.img (se crea vacío)
                                 # escribe versiones sin parar; cortá con Ctrl-A X
make run-almacen MODO=verificar  # muestra la versión recuperada y si está entera
```

Salida de `MODO=verificar`:

```
[almacen] almacen: montado (secuencia 213, ranura 0, 2 objetos)
[almacen] almacen:   dueño 1 "contador": versión 211, 16384 bytes
[escritor] escritor: recuperé "contador" en la versión 211: consistente (16384 bytes)
[curioso] curioso: escribir con la copia de solo lectura: rechazado ... ok
```

Otros modos (`MODO=...`): `escribir:N` (N versiones y sale), `caer:N`
(caída simulada antes del superbloque de la escritura N), `ajeno` (para
probar con un disco que no es del almacén). Para empezar de cero, borrá
`build/almacen.img`.

## FAT32 (más adelante)

Se puede sumar sin rehacer nada: el driver habla un protocolo de bloques
genérico, así que una cell `fat32` (lectora, para la tarjeta SD de la
Raspberry Pi) se conecta al mismo `virtblk` (o al driver de la SD) igual
que `almacen`. El almacén nunca formatea un disco que no está vacío ni es
suyo: con un FAT en el disco se niega y no escribe nada (lo prueba
`smoke-almacen`). Para tener los dos en un mismo disco haría falta una cell
de particiones que le dé a cada uno solo su rango de bloques.

## Límites (por ahora)

* Un objeto se escribe entero (hasta 64 KiB); no hay escrituras parciales
  ni borrado. Hasta 30 objetos y 64 MiB de disco.
* Un pedido a la vez, sin interrupciones (el driver y el almacén sondean).
* Los hashes detectan daños y escrituras a medias, no a alguien que
  reescribe el disco a propósito. Ver `docs/amenazas.md`.
* Las raíces las reparte el kernel al arrancar. Un supervisor que las cree
  (y las cuotas por dueño) quedan para después.
