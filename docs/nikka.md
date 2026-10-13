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
- Один стек ядра на процессор и выполнение до конца. В OSel у каждого потока свой стек ядра ([потоки](architecture/core/threads.md)).

## Какие тесты OSel выросли из лаб

| Лаба | Что проверяет | Тесты OSel |
|---|---|---|
| 1-hw-4, блокировки и прерывания | флаг прерываний возвращается после блокировки, при вложенных блокировках — только на внешней, неудачный `try_lock` его не трогает | `src/sync.rs`: `test_lock_disables_and_restores_interrupts`, `test_lock_keeps_interrupts_off_if_they_were_off`, `test_nested_locks_restore_at_the_outermost`, `test_failed_try_lock_leaves_interrupts_alone` |
| 2-mm-1, аллокатор страниц | все выданные страницы действительно свободны: их заполняют мусором, и ядро продолжает работать; после освобождения всех счётчик вернулся | `tests/frame_exhaustion.rs`: `every_frame_once`, `kernel_still_works_after_the_garbage` |
| 2-mm-1 | освобождённая страница выдаётся первой (LIFO), двойное освобождение — паника | `osel-core/src/frames.rs`: `a_freed_frame_is_reused_right_away`, `double_free_panics`; `src/memory.rs`: `test_freed_frame_is_reused`; `tests/double_free.rs` |
| 2-mm-4, отображение страниц | флаги промежуточных таблиц | `osel-core/src/memory.rs`: `intermediate_entries_get_only_access_flags`, `intermediate_entries_are_reused_and_allow_everything` |
| 2-mm-4 | сброс TLB при переотображении, нулевая запись после снятия, огромные страницы | `src/memory.rs`: `test_remapped_page_shows_the_new_frame`, `test_unmapped_entry_is_cleared`, `test_translation_through_huge_pages` |
| 2-mm-5, адресное пространство | ядро и программы разделены | `osel-core/src/memory.rs`: `kernel_and_user_are_separated` |
| 2-mm-5 | отображение поверх отображённой страницы — ошибка, старая страница цела | `osel-core/src/address_space.rs`: `mapping_over_a_mapped_page_rolls_back` |
| 2-mm-6, 2-mm-7, копирование и удаление поддеревьев | у нового пространства только верхняя половина ядра, страницы одного не видны в другом, нехватка страниц на каждом шаге без утечек, удаление возвращает всё, снятие освобождает ровно опустевшие таблицы | `osel-core/src/address_space.rs`: `a_new_space_shares_the_kernel_half_only`, `spaces_do_not_see_each_others_pages`, `running_out_of_frames_at_every_step_leaks_nothing`, `destroy_frees_everything_of_the_lower_half`, `unmapping_frees_the_emptied_tables`; `src/memory.rs`: `test_out_of_frames_at_every_step`, `test_kernel_addresses_are_the_same_in_a_user_space` |
| 4-process-2, загрузка ELF | плохие диапазоны и порядок сегментов, W+X — ошибка, права общей страницы, загрузка в поддельную память с проверкой каждого байта, создание процесса при нехватке страниц на каждом шаге | `osel-core/src/elf.rs`: `bad_segments_are_enoexec`, `a_shared_page_gets_both_sets_of_flags`, `loading_maps_zeroed_pages_and_copies_the_file`, `corrupted_files_never_panic`; `src/program.rs`: `test_loading_out_of_frames_at_every_step` |
| 4-process-1, 4-process-4, кольцо 3 | исключение программы с нулевым стеком не задевает ядро, все регистры программы сохраняются при исключении, исключение программы завершает её, а не ядро | `src/user.rs`: `test_a_program_without_a_stack`, `test_a_fault_saves_every_register`, `test_forbidden_instructions_end_the_program` |
| 4-process-5, системные вызовы | вызовы без стека (rsp = 0) не роняют ядро: стек меняется до включения прерываний | `src/syscall.rs`: `test_calls_without_a_stack`, `test_interrupts_come_on_after_the_stack_switch` |
| 4-process-3, проверка памяти от программы | диапазоны из 1–4 страниц с «дыркой», границы около четвертей страницы | `osel-core/src/user_mem.rs`: `every_page_of_the_range_is_checked` |
| 4-process-6, 4-process-7, 4-process-8, вытеснение и планировщик | все регистры и флаги со сдвоенными переходами переживают вытеснения, бесконечный цикл вытесняется, `sched_yield` переключает | `src/scheduler.rs`: `test_two_threads_keep_their_context`, `test_an_endless_loop_is_preempted`, `test_sched_yield` |
| 4-process, изоляция ring 3 | программа не достаёт до памяти ядра и чужих процессов, плохие указатели в вызовах — `EFAULT`, наводнение `clone` собрано без утечек | `src/security.rs`: `test_touching_the_kernel_faults_the_program`, `test_kernel_pointers_to_syscalls_are_efault`, `test_mapping_calls_refuse_forbidden_ranges`, `test_a_clone_flood_is_contained`, `test_processes_do_not_share_memory` |
| 7-um-2, память программы | свежая память нулевая, точные флаги страниц по правам, неудачное изменение не меняет ничего, огромный пустой диапазон обрабатывается быстро | `osel-core/src/mmap.rs`: `anonymous_memory_is_lazy_and_zero`, `flags_and_rights`, `mprotect_over_a_hole_changes_nothing`, `random_calls_match_a_model`; `src/syscall.rs`: `test_mmap_memory_is_lazy_and_zero`, `test_munmap_takes_the_pages` |
| 5-sync-2, спинлок | взаимоблокировка сообщается с местами захвата и ожидания, инвариант держится под конкуренцией | `osel-core/src/sync.rs`: `deadlock_is_reported_with_both_places`, `invariant_holds_under_contention`; `tests/deadlock.rs`, `tests/memory_reentrancy.rs` |
| 3-allocator, проверка утечек | после каждого теста куча возвращается в исходное состояние | `src/testosterone.rs` (проверка в запуске каждого теста), `tests/leak_detection.rs` |
| 2-mm, `forbid_frame_leaks` | после каждого теста число выданных физических страниц то же | `src/testosterone.rs`: `forbid_frame_leaks`; `tests/frame_leak_detection.rs` |

## Что ещё стоит перенести

- 1-hw-1 — разбор форматов часов реального времени (BCD, 12-часовой режим), когда появится драйвер часов. См. [техдолг](debt.md).
- 2-mm-1 — счётчики ссылок на страницы, когда появятся общие страницы.
- 4-process — таблица процессов с эпохами номеров (загрузка ELF, кольцо 3, системные вызовы и планировщик уже перенесены).
