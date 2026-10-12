---
description: Обзор OSel — из каких крейтов и модулей состоит система, как она загружается и как устроено адресное пространство.
---

# Обзор устройства

**Для кого:** разработчик и агент, который впервые открывает код.

**Коротко:** OSel — ядро на Rust для x86_64, `no_std`, загружается через `bootloader 0.11` в режимах UEFI и BIOS. Код разделён на ядро (всё, что трогает железо), `osel-core` (чистая логика, тестируется на хост-машине) и `runner` (инструмент, который собирает образ диска и запускает QEMU). Ниже карта модулей: у каждого модуля есть страница-владелец.

## Крейты {#crates}

| Крейт | Где | Под что собирается | Что в нём |
|---|---|---|---|
| ядро `osel` | `src/`, `tests/` | `x86_64-unknown-none` | библиотека ядра и бинарник `OSel`; интеграционные тесты — отдельные тестовые ядра |
| `osel-core` | `osel-core/` | хост и ядро | логика без обращения к железу: консоль, аллокаторы, карта памяти, клавиатура, отчёты об исключениях, спинлок |
| `osel-runner` | `runner/` | хост | раннер cargo: образ диска, запуск QEMU, разбор результатов тестов |
| `docs-check` | `tools/docs-check/` | хост | тесты, которые сверяют документацию с кодом, см. [как писать документацию](../guides/writing-docs.md#checks) |

`osel-core` — участник workspace. `runner` и `tools/docs-check` исключены из него: runner запускается вложенным cargo, пока сборка ядра держит блокировку папки target.

## Карта модулей {#modules}

Имена модулей ядра — игра слов, поэтому таблица обязательна к прочтению.

| Модуль | Где | Что делает | Страница |
|---|---|---|---|
| `avganec` | `src/avganec.rs` | консоль на framebuffer: `print!`, `println!`, экран паники | [Консоль](drivers/console.md) |
| `cereal` | `src/cereal.rs` | последовательный порт COM1: `serial_print!`, аварийный вывод | [Последовательный порт](drivers/serial.md) |
| `keyboard` | `src/keyboard.rs` | PS/2: скан-коды из прерывания в буфер символов | [Клавиатура](drivers/keyboard.md) |
| `interrupts` | `src/interrupts.rs` | IDT, обработчики исключений, PIC, таймер, ложные IRQ | [Прерывания](core/interrupts.md) |
| `gdt` | `src/gdt.rs` | GDT, TSS, стеки IST для double fault, NMI и machine check | [Прерывания](core/interrupts.md) |
| `memory` | `src/memory.rs` | таблица страниц, аллокатор физических страниц, `with_memory` | [Память](core/memory.md) |
| `allocator` | `src/allocator.rs` | куча ядра и глобальный аллокатор | [Память](core/memory.md#heap) |
| `sync` | `src/sync.rs` | `IrqSpinlock` — блокировка, выключающая прерывания | [Блокировки](core/locking.md) |
| `qemuno` | `src/qemuno.rs` | выход из QEMU через устройство `isa-debug-exit` | [Как устроены тесты](testing.md) |
| `testosterone` | `src/testosterone.rs` | свой тестовый фреймворк, `panic_test!`, проверка утечек | [Как устроены тесты](testing.md) |

Модули `osel-core`:

| Модуль | Где | Что делает | Страница |
|---|---|---|---|
| `console` | `osel-core/src/console.rs` | отрисовка текста в любой байтовый буфер | [Консоль](drivers/console.md) |
| `keyboard` | `osel-core/src/keyboard.rs` | `RingBuffer`, `ScancodeDecoder` | [Клавиатура](drivers/keyboard.md) |
| `pit` | `osel-core/src/pit.rs` | делитель таймера, маски PIC, распознавание ложных IRQ | [Прерывания](core/interrupts.md#pic-pit) |
| `memory` | `osel-core/src/memory.rs` | раскладка адресов, `RegionFrameAllocator`, `map_range` | [Память](core/memory.md) |
| `heap` | `osel-core/src/heap.rs` | `FixedSizeBlockAllocator` | [Память](core/memory.md#heap) |
| `exception` | `osel-core/src/exception.rs` | текст отчёта об исключении, `FixedBuf` | [Прерывания](core/interrupts.md#exceptions) |
| `sync` | `osel-core/src/sync.rs` | `Spinlock` с отчётом о взаимоблокировке | [Блокировки](core/locking.md) |

## Путь загрузки {#boot}

1. Загрузчик (`bootloader 0.11`) через UEFI или BIOS загружает ELF ядра в верхнюю половину адресов, отображает всю физическую память и framebuffer и передаёт управление в `kernel_main` (`src/main.rs`). Настройки загрузчика — `BOOTLOADER_CONFIG` в `src/lib.rs`.
2. `avganec::init` забирает framebuffer, на экране появляется «OSel <версия> (commit <хеш>) woke up!». Хеш коммита встраивает при сборке `build.rs`.
3. `osel::init` (`src/lib.rs`), строго один раз:
   1. выключает прерывания;
   2. `gdt::init` — своя GDT, сегментные регистры, пустая LDT, TSS;
   3. `interrupts::init_idt` — IDT;
   4. `memory::init` — таблица страниц, очистка нижней половины, страницы-охранники стеков IST, аллокатор страниц;
   5. `allocator::init_heap` — куча;
   6. `interrupts::init_pics` — сброс буфера клавиатуры, PIC, таймер на 100 Гц;
   7. включает прерывания.
4. Ядро печатает «OSel ready to go!» на экран и в последовательный порт, затем `echo_keyboard` печатает нажатые клавиши и спит в `hlt`, пока нет ввода.

После `osel::init` то, что передал загрузчик (карта памяти, ramdisk, таблицы ACPI), доступно через `osel::boot_info()`.

## Адресное пространство {#address-space}

| Область | Адреса |
|---|---|
| Программы (пока пусто) | нижняя половина, до `USER_END` |
| Всё, что размещает загрузчик: ядро, стек, сведения о загрузке, framebuffer, отображение физической памяти | `KERNEL_DYNAMIC_START..=KERNEL_DYNAMIC_END` |
| Куча ядра, 1 МБ | с `HEAP_START` |

Подробности, инварианты и грабли — на странице [Память](core/memory.md).

## Связанные решения {#decisions}

- [0001 — bootloader 0.11 и UEFI](../decisions/0001-bootloader-0.11-and-uefi.md)
- [0002 — osel-core и тесты на хосте](../decisions/0002-osel-core-host-tests.md)
- [0006 — ядро в верхней половине](../decisions/0006-higher-half-kernel.md)

_Сверено с кодом: 2026-10-12, коммит 2dee8ac._
