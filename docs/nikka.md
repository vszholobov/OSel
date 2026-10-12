---
description: Курс ШАДа Nikka — как OSel им пользуется, правило лицензии AGPL и какие тесты OSel выросли из лаб курса.
---

# Курс ШАДа Nikka

**Для кого:** для тех, кто хочет заглянуть в курс за идеями или перенести из него тест.

**Коротко:** Nikka — учебная ОС на Rust из курса ШАДа «Архитектура компьютера и операционные системы», с тем же стеком, что у OSel (`bootloader` 0.11, `x86_64-unknown-none`). Из неё можно брать идеи, устройство подсистем и замысел тестов. Код копировать нельзя: курс под лицензией AGPL-3.0.

## Ссылки

- [Описание лаб](https://sergey-v-galtsev.gitlab.io/labs-description/lab/book/index.html) — на русском.
- [Публичный репозиторий](https://gitlab.com/sergey-v-galtsev/nikka-public). В нём заготовки с пропусками «TODO: your code here», но описания, интерфейсы и тесты полные.

## Правило лицензии

{% note alert %}

Код Nikka распространяется под AGPL-3.0. Скопированный код сделал бы OSel производной работой под той же лицензией. Поэтому из курса берём только идеи, устройство подсистем и замысел тестов, а код пишем сами.

{% endnote %}

## Чем курс отличается от OSel

- Свой интерфейс системных вызовов в духе экзоядра, `fork` в пространстве пользователя. OSel реализует системные вызовы Linux ([решение 0005](decisions/0005-linux-abi-for-go.md)).
- Один стек ядра на процессор и выполнение до конца. OSel планирует стек ядра на каждый поток ([этап 2](design/stage-2-user-mode.md)).

## Какие тесты OSel выросли из лаб

| Лаба | Что проверяет | Тесты OSel |
|---|---|---|
| 1-hw-4, блокировки и прерывания | флаг прерываний возвращается после блокировки, при вложенных блокировках — только на внешней, неудачный `try_lock` его не трогает | `src/sync.rs`: `test_lock_disables_and_restores_interrupts`, `test_lock_keeps_interrupts_off_if_they_were_off`, `test_nested_locks_restore_at_the_outermost`, `test_failed_try_lock_leaves_interrupts_alone` |
| 2-mm-1, аллокатор страниц | все выданные страницы действительно свободны: их заполняют мусором, и ядро продолжает работать; после освобождения всех счётчик вернулся | `tests/frame_exhaustion.rs`: `every_frame_once`, `kernel_still_works_after_the_garbage` |
| 2-mm-1 | освобождённая страница выдаётся первой (LIFO), двойное освобождение — паника | `osel-core/src/frames.rs`: `a_freed_frame_is_reused_right_away`, `double_free_panics`; `src/memory.rs`: `test_freed_frame_is_reused`; `tests/double_free.rs` |
| 2-mm-4, отображение страниц | флаги промежуточных таблиц | `osel-core/src/memory.rs`: `intermediate_entries_get_only_access_flags`, `intermediate_entries_are_reused_and_widened` |
| 2-mm-4 | сброс TLB при переотображении, нулевая запись после снятия, огромные страницы | `src/memory.rs`: `test_remapped_page_shows_the_new_frame`, `test_unmapped_entry_is_cleared`, `test_translation_through_huge_pages` |
| 2-mm-5, адресное пространство | ядро и программы разделены | `osel-core/src/memory.rs`: `kernel_and_user_are_separated` |
| 5-sync-2, спинлок | взаимоблокировка сообщается с местами захвата и ожидания, инвариант держится под конкуренцией | `osel-core/src/sync.rs`: `deadlock_is_reported_with_both_places`, `invariant_holds_under_contention`; `tests/deadlock.rs`, `tests/memory_reentrancy.rs` |
| 3-allocator, проверка утечек | после каждого теста куча возвращается в исходное состояние | `src/testosterone.rs` (проверка в запуске каждого теста), `tests/leak_detection.rs` |
| 2-mm, `forbid_frame_leaks` | после каждого теста число выданных физических страниц то же | `src/testosterone.rs`: `forbid_frame_leaks`; `tests/frame_leak_detection.rs` |

## Что ещё стоит перенести

- 1-hw-1 — разбор форматов часов реального времени (BCD, 12-часовой режим), когда появится драйвер часов. См. [техдолг](debt.md).
- 2-mm-1 — счётчики ссылок на страницы, когда появятся общие страницы.
- 2-mm-6, 2-mm-7 — копирование и удаление поддеревьев таблиц страниц, для адресных пространств этапа 2.
- 4-process — пользовательский режим, системные вызовы, планировщик.
