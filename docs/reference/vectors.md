---
description: Справочник векторов прерываний OSel — каждая запись IDT, её обработчик, стек IST, код ошибки и что происходит.
---

# Векторы прерываний

**Для кого:** все, кто читает отчёт об исключении или добавляет обработчик.

**Коротко:** таблица всех записей IDT из `src/interrupts.rs`. Исключения процессора в ядре, кроме точки останова, заканчиваются экраном паники через `fatal_exception`. То же исключение из кольца 3 завершает программу, кроме векторов со стеком IST, см. [пользовательский режим](../architecture/core/user-mode.md#traps). Аппаратные прерывания начинаются с вектора 32. Обработчик есть у каждого исключения, кроме зарезервированных векторов. Вектор без обработчика даёт «SEGMENT NOT PRESENT» с кодом ошибки, в котором указан номер вектора. Как это устроено — на странице [прерывания](../architecture/core/interrupts.md).

## Исключения процессора {#exceptions}

Поле IDT — имя поля в `InterruptDescriptorTable` крейта `x86_64`, как в коде. Код ошибки «селектор» расшифровывается в отчёте как запись GDT, LDT или IDT.

| Вектор | Поле IDT | Имя в отчёте | IST | Код ошибки | Что происходит |
|---|---|---|---|---|---|
| 0 | `divide_error` | DIVIDE ERROR | — | нет | паника |
| 1 | `debug` | DEBUG | — | нет | паника |
| 2 | `non_maskable_interrupt` | NON-MASKABLE INTERRUPT | 1 | нет | паника на своём стеке |
| 3 | `breakpoint` | — | — | нет | счётчик `breakpoints()` и строка через `try_print`, выполнение продолжается |
| 4 | `overflow` | OVERFLOW | — | нет | паника |
| 5 | `bound_range_exceeded` | BOUND RANGE EXCEEDED | — | нет | паника |
| 6 | `invalid_opcode` | INVALID OPCODE | — | нет | паника |
| 7 | `device_not_available` | DEVICE NOT AVAILABLE | — | нет | паника |
| 8 | `double_fault` | DOUBLE FAULT | 0 | число | паника на своём стеке |
| 10 | `invalid_tss` | INVALID TSS | — | селектор | паника |
| 11 | `segment_not_present` | SEGMENT NOT PRESENT | — | селектор | паника; так же выглядит вектор без обработчика |
| 12 | `stack_segment_fault` | STACK SEGMENT FAULT | — | селектор | паника |
| 13 | `general_protection_fault` | GENERAL PROTECTION FAULT | — | селектор | паника |
| 14 | `page_fault` | PAGE FAULT | — | флаги page fault | паника с адресом из CR2 и причиной словами |
| 16 | `x87_floating_point` | X87 FLOATING POINT | — | нет | паника |
| 17 | `alignment_check` | ALIGNMENT CHECK | — | число | паника |
| 18 | `machine_check` | MACHINE CHECK | 2 | нет | паника на своём стеке |
| 19 | `simd_floating_point` | SIMD FLOATING POINT | — | нет | паника |
| 20 | `virtualization` | VIRTUALIZATION | — | нет | паника |
| 21 | `cp_protection_exception` | CONTROL PROTECTION | — | число | паника |
| 28 | `hv_injection_exception` | HYPERVISOR INJECTION EXCEPTION | — | нет | паника |
| 29 | `vmm_communication_exception` | VMM COMMUNICATION EXCEPTION | — | число | паника |
| 30 | `security_exception` | SECURITY EXCEPTION | — | число | паника |

Из кольца 3 любое исключение, кроме векторов 2, 8 и 18 на стеках IST, завершает программу вместо паники; для программ #MF (16) и #XM (19) — обычные ошибки вычислений, см. [x87 и SSE](../architecture/core/user-mode.md#fpu). `int3` и любая `int N` из программы дают #GP: у всех шлюзов DPL 0.

Вектор 256 в `TrapFrame` — не прерывание, а вход через `syscall`, см. [пользовательский режим](../architecture/core/user-mode.md#syscall).

Векторы 9, 15, 22–27 и 31 зарезервированы процессором, обработчиков у них нет. Если такой вектор всё же придёт (например, через `int`), он закончится «SEGMENT NOT PRESENT» с номером вектора в коде ошибки.

Номера IST — индексы в TSS: 0 — `DOUBLE_FAULT_IST_INDEX`, 1 — `NMI_IST_INDEX`, 2 — `MACHINE_CHECK_IST_INDEX` (`src/gdt.rs`).

## Аппаратные прерывания {#hardware}

Векторы задаёт перечисление `InterruptIndex`: ведущий PIC начинается с `PIC_1_OFFSET` = 32, ведомый — с `PIC_2_OFFSET` = 40.

| Вектор | `InterruptIndex` | IRQ | Обработчик | Что происходит |
|---|---|---|---|---|
| 32 | `Timer` | 0 | `timer_interrupt_handler` | счётчик `ticks()`, 100 раз в секунду, подтверждение PIC, затем переключение потоков (`scheduler::tick`) |
| 33 | `Keyboard` | 1 | `keyboard_interrupt_handler` | скан-код из порта `0x60` в буфер клавиатуры, подтверждение PIC |
| 39 | `SpuriousMaster` | 7 | `spurious_master_handler` | если бит в регистре ISR не выставлен — ложное прерывание, счётчик `spurious_interrupts()`, без подтверждения; иначе подтверждение |
| 47 | `SpuriousSlave` | 15 | `spurious_slave_handler` | если ложное — счётчик и подтверждение только ведущему PIC; иначе подтверждение |

Из линий PIC открыты только IRQ 0 и IRQ 1 (`ENABLED_IRQS`). Ложные IRQ 7 и 15 приходят и на закрытых линиях.

## Как добавить вектор {#add}

1. Обработчик в `src/interrupts.rs` и строка в построении IDT.
2. Для исключения — вызов `fatal_exception` с именем и подходящим видом `Details`. Если исключение может вызвать программа, ещё заглушка `entry_stub` и `through_stub` в построении IDT, имя — в `exception_name` (`osel-core/src/trap.rs`).
3. Строка в этой таблице — `cargo docs-check` проверяет, что здесь упомянуто каждое поле IDT и каждый вариант `InterruptIndex`.
4. Тест: интеграционный `panic_test!`, см. [рецепт тестов](../guides/testing.md).

_Сверено с кодом: 2026-10-12, коммит 347786c._
