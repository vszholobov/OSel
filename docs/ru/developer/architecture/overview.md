---
description: Обзор OSel — из каких крейтов и модулей состоит система, как она загружается и как устроено адресное пространство.
---

# Обзор устройства

Язык: [Русский](overview.md) · [English](../../../en/developer/architecture/overview.md)

**Для кого:** разработчик и агент, который впервые открывает код.

**Коротко:** OSel — ядро на Rust для x86_64, `no_std`, загружается через `bootloader 0.11` в режимах UEFI и BIOS. Код разделён на ядро (всё, что трогает железо), `osel-core` (чистая логика, тестируется на хост-машине) и `runner` (инструмент, который собирает образ диска и запускает QEMU). Ниже карта модулей: у каждого модуля есть страница-владелец.

## Крейты {#crates}

| Крейт | Где | Под что собирается | Что в нём |
|---|---|---|---|
| ядро `osel` | `src/`, `tests/` | `x86_64-unknown-none` | библиотека ядра и бинарник `OSel`; интеграционные тесты — отдельные тестовые ядра |
| `osel-core` | `osel-core/` | хост и ядро | логика без обращения к железу: консоль, аллокаторы, карта памяти, клавиатура, отчёты об исключениях, спинлок |
| `osel-runner` | `runner/` | хост | раннер cargo: ramdisk из папки `ramdisk/` и программ из `user/`, образ диска, запуск QEMU, разбор результатов тестов; зависит от `osel-core` ради формата cpio |
| программы | `user/<имя>/` | `x86_64-unknown-none` | программы для кольца 3 на ассемблере, runner кладёт их в `/bin` ramdisk, см. [решение 0012](../decisions/0012-syscall-entry-and-user-programs.md) |
| программы на Go | `user-go/<имя>/` | `linux/amd64`, статическая сборка | runner кладёт их в `/bin` ramdisk; `gohello` проверяет горутины и таймеры, см. [программы](core/programs.md#go) |
| `docs-check` | `tools/docs-check/` | хост | тесты, которые сверяют документацию с кодом, см. [как писать документацию](../guides/writing-docs.md#checks) |

`osel-core` — участник workspace. `runner` и `tools/docs-check` исключены из него: runner запускается вложенным cargo, пока сборка ядра держит блокировку папки target. Программы из `user/` — отдельные крейты со своим `[workspace]` и своей папкой `user/target`.

## Карта модулей {#modules}

Имена модулей ядра — игра слов, поэтому таблица обязательна к прочтению.

| Модуль | Где | Что делает | Страница |
|---|---|---|---|
| `avganec` | `src/avganec.rs` | консоль на framebuffer: `print!`, `println!`, экран паники | [Консоль](drivers/console.md) |
| `cereal` | `src/cereal.rs` | последовательный порт COM1: `serial_print!`, аварийный вывод | [Последовательный порт](drivers/serial.md) |
| `keyboard` | `src/keyboard.rs` | инициализация PS/2, IRQ, резервный опрос, буфер и диагностика | [Клавиатура](drivers/keyboard.md) |
| `interrupts` | `src/interrupts.rs` | IDT, обработчики исключений, PIC, таймер, ложные IRQ | [Прерывания](core/interrupts.md) |
| `gdt` | `src/gdt.rs` | GDT с сегментами кольца 3, TSS, стеки IST для double fault, NMI и machine check | [Прерывания](core/interrupts.md) |
| `fpu` | `src/fpu.rs` | включение SSE и x87 (CR0, CR4), `fxsave64` и `fxrstor64` | [Пользовательский режим](core/user-mode.md#fpu) |
| `syscall` | `src/syscall.rs` | вход через `syscall` и вызовы Linux для ввода-вывода, памяти, потоков и рантайма Go | [Пользовательский режим](core/user-mode.md#syscall), [справочник](../reference/syscalls.md) |
| `scheduler` | `src/scheduler.rs` | потоки, процессы, переключение, вытеснение по таймеру | [Потоки и планировщик](core/threads.md) |
| `user` | `src/user.rs` | запуск программы в кольце 3, возврат при исключении | [Пользовательский режим](core/user-mode.md) |
| `time` | `src/time.rs` | монотонные и реальные часы, сон до срока (`SLEEPERS`) | [потоки](core/threads.md#time) |
| `futex` | `src/futex.rs` | `futex`: ожидание и пробуждение потоков по адресу (`FUTEXES`) | [потоки](core/threads.md#futex) |
| `poll` | `src/poll.rs` | блокирующий `epoll_pwait` и пробуждение при записи в eventfd (`POLL_WAITERS`) | [открытые файлы](core/fs.md#epoll) |
| `pipe` | `src/pipe.rs` | концы анонимного канала, блокирующее чтение и запись, пробуждение при закрытии | [Каналы](core/fs.md#pipes) |
| `security` | `src/security.rs` | враждебные программы-тесты изоляции ring 3 (только тесты) | [изоляция программ](core/security.md) |
| `memory` | `src/memory.rs` | таблица страниц, пул физических страниц, `with_memory`, `UserSpace` | [Память](core/memory.md) |
| `program` | `src/program.rs` | загрузка программы из файловой системы в `UserSpace`, стек главного потока | [Программы](core/programs.md) |
| `shell` | `src/shell.rs` | интерактивные команды и запуск программ с возвратом в приглашение | [Оболочка](shell.md) |
| `fs` | `src/fs.rs` | монтирует ramdisk как корень с `/dev`, `lookup`, `file_for`, `read_file` | [Файлы и ramdisk](core/fs.md) |
| `tty` | `src/tty.rs` | терминал: экран и порт, ввод PS/2 и COM1, режимы и очередь ожидающих потоков | [Файлы и ramdisk](core/fs.md#open-files) |
| `allocator` | `src/allocator.rs` | куча ядра и глобальный аллокатор | [Память](core/memory.md#heap) |
| `sync` | `src/sync.rs` | `IrqSpinlock` — блокировка, выключающая прерывания | [Блокировки](core/locking.md) |
| `qemuno` | `src/qemuno.rs` | выход из QEMU через устройство `isa-debug-exit` | [Как устроены тесты](testing.md) |
| `testosterone` | `src/testosterone.rs` | свой тестовый фреймворк, `panic_test!`, проверка утечек | [Как устроены тесты](testing.md) |

Модули `osel-core`:

| Модуль | Где | Что делает | Страница |
|---|---|---|---|
| `console` | `osel-core/src/console.rs` | отрисовка текста в любой байтовый буфер | [Консоль](drivers/console.md) |
| `shell` | `osel-core/src/shell.rs` | ограниченный редактор строки, разбор аргументов и имён программ | [Оболочка](shell.md) |
| `font` | `osel-core/src/font.rs` | встроенные растры шрифта 8×16 и 16×32, выбор размера и глифа | [Консоль](drivers/console.md#font) |
| `keyboard` | `osel-core/src/keyboard.rs` | `RingBuffer`, `ScancodeDecoder` | [Клавиатура](drivers/keyboard.md) |
| `ps2` | `osel-core/src/ps2.rs` | протокол инициализации i8042/клавиатуры, ограниченные ожидания, ACK/RESEND | [Клавиатура](drivers/keyboard.md) |
| `pit` | `osel-core/src/pit.rs` | делитель таймера, маски PIC, распознавание ложных IRQ | [Прерывания](core/interrupts.md#pic-pit) |
| `memory` | `osel-core/src/memory.rs` | раскладка адресов, `RegionFrameAllocator`, `map_range`, `unmap_range`, `protect_range`, `prune_empty_tables` | [Память](core/memory.md) |
| `address_space` | `osel-core/src/address_space.rs` | `AddressSpace` — таблица уровня 4 программы с общей верхней половиной | [Память](core/memory.md#address-spaces) |
| `vma` | `osel-core/src/vma.rs` | `VmaSet` — области адресного пространства программы с правами | [Память](core/memory.md#areas) |
| `mmap` | `osel-core/src/mmap.rs` | `Mappings` — `mmap`, `munmap`, `mprotect`, `madvise` и страница при первом обращении | [Память](core/memory.md#areas) |
| `clock` | `osel-core/src/clock.rs` | тики таймера в наносекунды, `timespec`, дата CMOS в эпоху Unix | [потоки](core/threads.md#time) |
| `sleeper` | `osel-core/src/sleeper.rs` | очередь потоков, ждущих до срока | [потоки](core/threads.md#time) |
| `futex` | `osel-core/src/futex.rs` | очереди ожидания `futex` по адресу | [потоки](core/threads.md#futex) |
| `poll` | `osel-core/src/poll.rs` | `eventfd` и `epoll`: счётчик и список наблюдения | [открытые файлы](core/fs.md#epoll) |
| `pipe` | `osel-core/src/pipe.rs` | ограниченный буфер канала, EOF, EPIPE и готовность к вводу-выводу | [Каналы](core/fs.md#pipes) |
| `terminal` | `osel-core/src/terminal.rs` | Linux termios, буфер терминала, построчный ввод и эхо | [Файлы и ramdisk](core/fs.md#open-files) |
| `frames` | `osel-core/src/frames.rs` | интерфейс `FrameSource`, `FramePool` с освобождением страниц | [Память](core/memory.md#frames) |
| `heap` | `osel-core/src/heap.rs` | `FixedSizeBlockAllocator` | [Память](core/memory.md#heap) |
| `exception` | `osel-core/src/exception.rs` | текст отчёта об исключении, `FixedBuf` | [Прерывания](core/interrupts.md#exceptions) |
| `sync` | `osel-core/src/sync.rs` | `Spinlock` с отчётом о взаимоблокировке | [Блокировки](core/locking.md) |
| `errno` | `osel-core/src/errno.rs` | коды ошибок Linux `Errno`, `KResult` | [Файлы и ramdisk](core/fs.md) |
| `block` | `osel-core/src/block.rs` | интерфейс `BlockDevice`, `MemBlockDevice`, `read_bytes` | [Файлы и ramdisk](core/fs.md#layers) |
| `vfs` | `osel-core/src/vfs.rs` | интерфейсы `FileSystem` и `Inode`, разрешение путей | [Файлы и ramdisk](core/fs.md#resolve) |
| `elf` | `osel-core/src/elf.rs` | разбор и загрузка статических ELF64 через `ProgramMemory`, `ElfBuilder` | [Программы](core/programs.md#elf) |
| `initial_stack` | `osel-core/src/initial_stack.rs` | стартовый стек: argv, envp, auxv | [Программы](core/programs.md#initial-stack) |
| `syscall` | `osel-core/src/syscall.rs` | номера вызовов Linux, аргументы, `-errno`, выбор пути назад, `Utf8Decoder` | [Системные вызовы](../reference/syscalls.md) |
| `fpu` | `osel-core/src/fpu.rs` | `FpuState` — область `fxsave64` и её начальное состояние | [Пользовательский режим](core/user-mode.md#fpu) |
| `sched` | `osel-core/src/sched.rs` | `SchedPolicy`, `RoundRobin`, места стеков ядра `StackSlots` | [Потоки и планировщик](core/threads.md) |
| `trap` | `osel-core/src/trap.rs` | `TrapFrame` — регистры программы при входе в ядро | [Пользовательский режим](core/user-mode.md#trap-frame) |
| `user_mem` | `osel-core/src/user_mem.rs` | проверка памяти, которую программа передала ядру | [Пользовательский режим](core/user-mode.md#user-mem) |
| `cpio` | `osel-core/src/cpio.rs` | файловая система `CpioFs` над архивом cpio, `CpioBuilder` | [Файлы и ramdisk](core/fs.md#cpio) |
| `devices` | `osel-core/src/devices.rs` | каталог `/dev` поверх корня, номера устройств | [Файлы и ramdisk](core/fs.md#devices) |
| `file` | `osel-core/src/file.rs` | `FileLike`, `OpenDescription`, `FdTable`, правила `openat`, `struct stat` | [Файлы и ramdisk](core/fs.md#open-files) |

## Путь загрузки {#boot}

1. Загрузчик (`bootloader 0.11`) через UEFI или BIOS загружает ELF ядра в верхнюю половину адресов, отображает всю физическую память и framebuffer и передаёт управление в `kernel_main` (`src/main.rs`). Настройки загрузчика — `BOOTLOADER_CONFIG` в `src/lib.rs`.
2. `avganec::init` забирает framebuffer, на экране и в последовательном порту появляется «OSel <версия> (commit <хеш>) woke up!». Хеш коммита встраивает при сборке `build.rs`; проверка готовых образов сверяет этот баннер с версией и коммитом релиза.
3. `osel::init` (`src/lib.rs`), строго один раз:
   1. выключает прерывания;
   2. `gdt::init` — своя GDT, сегментные регистры, пустая LDT, TSS;
   3. `fpu::init` — SSE и x87 для программ;
   4. `interrupts::init_idt` — IDT;
   5. `syscall::init` — STAR, LSTAR, SFMASK, включение `syscall`;
   6. `memory::init` — таблица страниц, очистка нижней половины, страницы-охранники стеков IST, пул страниц, таблицы уровня 3 для всей верхней половины;
   7. `allocator::init_heap` — куча;
   8. `memory::enable_frame_tracking` — битовая карта пула в куче, с этого момента страницы можно освобождать;
   9. `fs::init` — ramdisk монтируется как корень `/` с каталогом устройств `/dev`;
   10. `scheduler::init` — поток 0 для кода загрузки и поток простоя;
   11. `keyboard::init` — настройка контроллера и PS/2-клавиатуры; `interrupts::init_pics` — сброс оставшихся байтов, PIC, таймер на 100 Гц;
   12. `time::init`, `futex::init`, `poll::init` — часы и очереди ожидания;
   13. включает прерывания.
4. Ядро печатает `/etc/motd` из ramdisk, запускает `/bin/hello` (он печатает «Hello from ring 3!»), затем «OSel ready to go!» на экран и в последовательный порт. Далее запускается [оболочка](shell.md) с приглашением `osel>`; игра запускается командой `tetris` и после выхода возвращает в оболочку.

После `osel::init` то, что передал загрузчик (карта памяти, ramdisk, таблицы ACPI), доступно через `osel::boot_info()`.

## Адресное пространство {#address-space}

| Область | Адреса |
|---|---|
| Программы, у каждой своё адресное пространство | нижняя половина, до `USER_END` |
| Всё, что размещает загрузчик: ядро, стек, сведения о загрузке, framebuffer, ramdisk, отображение физической памяти | `KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END` |
| Куча ядра, 16 МБ | с `HEAP_START` |
| Стеки ядра потоков, по 64 КБ со страницей-охранником | с `STACKS_START` |

Подробности, инварианты и грабли — на странице [Память](core/memory.md).

## Связанные решения {#decisions}

- [0001 — bootloader 0.11 и UEFI](../decisions/0001-bootloader-0.11-and-uefi.md)
- [0002 — osel-core и тесты на хосте](../decisions/0002-osel-core-host-tests.md)
- [0006 — ядро в верхней половине](../decisions/0006-higher-half-kernel.md)
- [0007 — интерфейсы хранения и ramdisk в cpio](../decisions/0007-storage-interfaces.md)
- [0008 — пул физических страниц](../decisions/0008-frame-pool.md)
- [0009 — общая верхняя половина адресных пространств](../decisions/0009-shared-kernel-half.md)
- [0010 — свой загрузчик статических ELF](../decisions/0010-static-elf-loader.md)
- [0011 — запуск программы до исключения и заглушки входа](../decisions/0011-user-entry.md)
- [0012 — вход через syscall и программы из user/](../decisions/0012-syscall-entry-and-user-programs.md)
- [0013 — состояние x87 и SSE программы](../decisions/0013-fpu-state.md)
- [0014 — дескрипторы и устройства](../decisions/0014-file-descriptors.md)
- [0015 — потоки и вытеснение](../decisions/0015-threads.md)

_Сверено с кодом: 2026-10-13, ветка release-images._
