---
description: Проектирование этапа 2 — пользовательский режим, системные вызовы Linux, загрузка ELF из ramdisk и всё, что нужно рантайму Go.
---

# Этап 2: пользовательский режим и программы на Go

**Для кого:** для разработчика или агента, который начинает задачу этапа 2.

**Коротко:** этап 2 должен запустить статическую Go-программу «hello world» с горутинами и `time.Sleep`. Ниже — порядок работы по PR, интерфейсы, которые закладываем под расширение, проверки и известные ловушки. План сверен с курсом ШАДа ([Nikka](../nikka.md)), блогом Phil Opp, OSDev Wiki и ядрами Kerla, Asterinas и Maestro. Когда этап закроется, содержимое переедет в `architecture/` и решения, а здесь останется ссылка (правило — в [«Как писать документацию»](../guides/writing-docs.md#single-home)).

Перед началом прочитайте: [память](../architecture/core/memory.md), [прерывания](../architecture/core/interrupts.md), [блокировки](../architecture/core/locking.md), [файлы и ramdisk](../architecture/core/fs.md), [адресные пространства](../architecture/core/memory.md#address-spaces), [программы](../architecture/core/programs.md), решения [0005](../decisions/0005-linux-abi-for-go.md), [0006](../decisions/0006-higher-half-kernel.md), [0007](../decisions/0007-storage-interfaces.md), [0009](../decisions/0009-shared-kernel-half.md) и [0010](../decisions/0010-static-elf-loader.md).

## Принципы {#principles}

- **Сначала интерфейс, потом реализация.** Каждая подсистема — трейт в `osel-core` и одна реализация сейчас. Новый диск, часы или планировщик потом добавляются реализацией, без переделки вызывающего кода. Контракты берём у Asterinas, объём — у Kerla.
- **Ошибки — `Errno`** (`osel-core/src/errno.rs`). Системный вызов возвращает `-errno` без перевода.
- **Чистая логика — в `osel-core`** с тестами на хосте, включая случайные и сломанные входные данные. В QEMU проверяется только железо: переходы между кольцами, исключения, вытеснение.
- **Каждый тест проверяет утечки** не только кучи, но и физических страниц ([проверка утечек](../architecture/testing.md#leaks)).
- **Негативные тесты обязательны:** программа делает что-то плохое, ядро живо, ничего не утекло.

## Интерфейсы {#interfaces}

| Интерфейс | Что | Сейчас | Потом | Статус |
|---|---|---|---|---|
| `BlockDevice` | блоки по номеру | `MemBlockDevice` (ramdisk) | AHCI, NVMe, virtio-blk | сделано |
| `FileSystem`, `Inode` | дерево файлов | `CpioFs` | ext2, FAT, файлы устройств | сделано |
| `FileLike` | открытый файл: `read`, `write`, `poll`, `ioctl`; по умолчанию ошибка | файл из `Inode`, `/dev/tty`, eventfd, epoll | pipe, сокеты (`as_socket`) | шаг 7 |
| `FdTable` | дескрипторы процесса: наименьший свободный, `dup`, `O_CLOEXEC` | — | — | шаг 7 |
| `FrameSource` | выдать и вернуть физическую страницу | `FramePool`: список свободных страниц | счётчики ссылок для копирования при записи | сделано |
| `VmaSet` | области памяти программы: разрез, слияние, поиск места | анонимные области | файлы в памяти (`Backing::File`) | шаг 9 |
| `UserMem` | копирование из памяти программы и в неё с проверкой | обход таблиц страниц | — | шаг 4 |
| `ClockSource` | монотонное время | счётчик PIT | TSC, HPET, RTC | шаг 10 |
| `SchedPolicy` | кого запускать следующим | по кругу | приоритеты, несколько процессоров | шаг 8 |

Процесс и поток разделены сразу: потоки Go (`clone` с `CLONE_VM | CLONE_FILES`) делят `Arc<UserSpace>` и `Arc<FdTable>`.

**Не делаем на этом этапе:** запись в файловую систему, кэш страниц, права и пользователей, `fork` и копирование при записи, несколько процессоров, символьные ссылки, доставку сигналов.

## Порядок работы {#order}

Каждый шаг — отдельный PR со своей проверкой.

0. **Ramdisk и интерфейсы хранения.** Сделано: [файлы и ramdisk](../architecture/core/fs.md).
1. **Освобождение страниц.** Сделано: [пул страниц](../architecture/core/memory.md#frames), [решение 0008](../decisions/0008-frame-pool.md). `FrameGuard` перенесён в шаг 2.
2. **Адресное пространство.** Сделано: [адресные пространства](../architecture/core/memory.md#address-spaces), [решение 0009](../decisions/0009-shared-kernel-half.md). `map_range` обнуляет страницы всегда, `FrameGuard` не понадобился.
3. **ELF и стартовый стек.** Сделано: [программы](../architecture/core/programs.md), [решение 0010](../decisions/0010-static-elf-loader.md). RDX = 0 при входе в программу — на шаге 4.
4. **Пользовательский режим без системных вызовов.** Сегменты кольца 3, изменяемый TSS с `set_kernel_stack`, единый `TrapFrame`, вход через `iretq`. Исключение из кольца 3 завершает программу, а не ядро. `UserMem` с проверкой каждой страницы. **Проверка:** программа ставит узоры в регистры и обращается к нулевому адресу — ядро живо, регистры сохранены верно, прерывания включены.
5. **Системные вызовы `write`, `exit`.** Вход через `syscall`, таблица вызовов в `osel-core` и справочник `reference/syscalls.md` со сверкой в `cargo docs-check`. **Проверка:** программа на ассемблере из ramdisk печатает «hello». Плюс «пыточное» ядро, см. [тесты](#testing).
6. **SSE.** CR0 и CR4, область `fxsave` на поток с начальными FCW `0x037F` и MXCSR `0x1F80`. #XM из кольца 3 завершает программу.
7. **Файлы для программ.** `FileLike`, `FdTable`, `openat`, `read`, `close`, `fstat`, `/dev/tty` и `/dev/null`.
8. **Потоки и вытеснение.** Свой стек ядра на поток со страницей-охранником из отдельной области адресов, `switch_to` сохраняет только callee-saved регистры, поток простоя с `sti; hlt`, отложенное освобождение стека завершившегося потока. FS — база TLS. **Проверка:** два потока с разными узорами во всех регистрах, XMM, MXCSR и FS — 50 вытеснений без порчи; бесконечный цикл вытесняется; оба счётчика растут.
9. **Память для Go.** `VmaSet`, `mmap` с подсказкой адреса без `MAP_FIXED`, `MAP_FIXED` поверх резерва `PROT_NONE`, `munmap`, `mprotect`, `madvise` как пустая операция; отложенное выделение страниц в обработчике page fault.
10. **Время и ожидание.** `clone`, `futex`, `nanosleep`, `clock_gettime`, `epoll` и `eventfd2`. `time.Sleep` требует их с первого таймера: Go вызывает `epoll_create1`, `eventfd2` и `epoll_ctl`, а запись в eventfd должна будить `epoll_pwait`.
11. **Go.** Остальные вызовы из [таблицы](#syscalls). **Проверка:** «hello world», затем горутины и `time.Sleep`.

## Вход в ядро {#entry}

- **Один процессор — без GS.** Заглушка `syscall` сохраняет стек программы и берёт стек ядра из статических ячеек. Иначе каждый вход из кольца 3 (таймер, клавиатура, исключения) требовал бы условного `swapgs`, а обработчики `x86-interrupt` его не делают. GS понадобится вместе с несколькими процессорами.
- SFMASK сбрасывает IF, DF, TF и AC. После смены стека прерывания включаются (`sti`): иначе блокирующие `futex` и `nanosleep` невозможны. Включить их до смены стека — page fault у адреса `0xFFFF_FFFF_FFFF_FFF8`.
- `sysretq` с неканоническим RCX на Intel даёт #GP в кольце 0 ещё на стеке программы. Перед `sysretq` проверять RCX, иначе возврат через `iretq`.
- Перед возвратом в программу обнулять регистры, которые вызов не возвращает: данные ядра не должны утечь.
- Адреса, которые программа передаёт в `wrmsr` (`arch_prctl(ARCH_SET_FS)`), проверять на `< USER_END`: неканонический адрес — #GP в ядре.
- Перед функциями, которые не возвращаются (`exit`, переключение потока), блокировки отпускать явно: деструкторы не выполнятся.
- Page fault не переносить на стек IST: стек IST общий и переиспользуется при повторном входе, а обработчик page fault будет блокироваться при отложенном выделении.

## Программы {#programs}

Загрузка и стартовый стек сделаны: [программы](../architecture/core/programs.md). Осталось: RDX при входе в программу — 0 (шаг 4).

## Системные вызовы для рантайма Go {#syscalls}

Ориентир — около 30 вызовов. Неподдержанные возвращают `ENOSYS`.

| Группа | Вызовы |
|---|---|
| Ввод-вывод | `write`, `read`, `openat`, `close`, `fstat`, `fcntl` |
| Память | `mmap`, `munmap`, `madvise`, `mprotect` |
| Потоки | `clone`, `futex`, `gettid`, `tgkill`, `sched_yield`, `sched_getaffinity`, `exit`, `exit_group` |
| Сигналы | `rt_sigaction`, `rt_sigprocmask`, `sigaltstack` |
| Время | `clock_gettime`, `nanosleep` |
| Прочее | `arch_prctl`, `getrandom`, `getpid`, `prlimit64`, `epoll_create1`, `epoll_ctl`, `epoll_pwait`, `pipe2`, `eventfd2` |

Что Go требует от них:

- Падают при любой ошибке: `rt_sigprocmask`, `sigaltstack`, `munmap`, `arch_prctl(ARCH_SET_FS)`. `rt_sigaction` должен вернуть 0 для всех сигналов, кроме 32, 33 и 64; обработчик достаточно запомнить. `sigaltstack` возвращает `SS_DISABLE`.
- `clone`: ребёнок получает `rax = 0` и новый `rsp`, остальные регистры — от родителя (Go держит в R9, R12, R13 свои указатели). Флаги Go: `VM | FS | FILES | SIGHAND | SYSVSEM | THREAD`, остальные сочетания — `EINVAL`.
- `sched_getaffinity` возвращает 8 и маску с битом 0: тогда GOMAXPROCS = 1.
- `tgkill(SIGURG)` — асинхронное вытеснение; делаем пустой операцией и передаём `GODEBUG=asyncpreemptoff=1`.
- `nanosleep` из `sysmon` приходит постоянно (20 мкс – 10 мс): сон должен блокировать поток, а не крутиться.
- Открытие `/sys/kernel/mm/transparent_hugepage/hpage_pmd_size` — `ENOENT` допустим.

**Ограничение этапа:** сигналы не доставляются. Разыменование nil в Go не превратится в панику, процесс просто завершится.

## Время {#time}

- Переключение по таймеру PIT 100 Гц (`TICK_HZ` в `src/interrupts.rs`).
- Часы: сначала тики PIT плюс защёлка счётчика PIT. TSC в QEMU без KVM ненадёжен, его добавить отдельной реализацией `ClockSource` после проверки.
- Очередь спящих потоков для `nanosleep` и тайм-аутов `futex` и `epoll` — в `osel-core`, на поддельных часах.

## Как тестировать {#testing}

- **Хост:** `FrameSource` поверх `Vec`, `VmaSet` со сверкой с простой моделью-битмапом, ELF и стартовый стек байт в байт, `FdTable`, eventfd и epoll на поддельных файлах, очереди `futex` и сна на поддельных часах, планировщик как конечный автомат. Фаззинг ELF сделан.
- **Тестовые программы** на ассемблере лежат в ramdisk. **Предложение:** исходники в папке tests/user (её ещё нет), сборка в `build.rs`, а runner кладёт результат в ramdisk; свой ramdisk у тестового ядра — в [техдолге](../debt.md). Помощник `run_user(path) -> код выхода` для тестов ядра.
- **«Пыточное» ядро:** плохой указатель в `write` и адрес ядра — `EFAULT`; номер 9999 — `ENOSYS`; `int 0x80`, `int3`, `hlt`, `cli`, `in`/`out`, `rdmsr`, переход на адрес ядра, запись в `.text`, исполнение из NX-страницы, деление на ноль, переполнение стека программы, `syscall` с поднятыми TF, AC и DF, `syscall` на последних байтах нижней половины (неканонический адрес возврата, тест `sysret_rip` из Linux). Для каждого — ядро живо, утечек нет.
- **Сохранение состояния:** после `getpid` изменились только RAX, RCX и R11; программа `check_context` из идеи Nikka сравнивает все регистры в цикле под вытеснением, каждое сравнение продублировано, чтобы прерывание попадало между `cmp` и `jcc`.
- **Утечки:** программа 100 раз подряд — число свободных страниц и размер кучи совпали.
- **Сверка с Linux в CI:** те же тестовые ELF запускаются в Linux, вывод и код выхода сравниваются. Для Go закрепить версию, `CGO_ENABLED=0 GOAMD64=v1` и эталонный список вызовов из `strace -f`.
- **Детерминизм:** режим runner с `-icount shift=N,align=off,sleep=off`, чтобы время зависело только от числа инструкций.

## Открытые вопросы {#questions}

- Как runner выбирает ramdisk для отдельного тестового ядра: по имени теста или по описанию в самом ядре.
- Нужен ли `pipe2` при наличии `eventfd2` (Go 1.21+ использует eventfd).

## Где смотреть {#links}

- Курс ШАДа Nikka: лабы 2-mm (освобождение страниц, адресное пространство), 4-process и 5-sync; правила — на странице [курса](../nikka.md).
- Ядра на Rust, совместимые с Linux: [Kerla](https://github.com/nuta/kerla), [Asterinas](https://github.com/asterinas/asterinas), [Maestro](https://github.com/maestro-os/maestro).
- Go: [runtime/os_linux.go](https://github.com/golang/go/blob/master/src/runtime/os_linux.go), [runtime/sys_linux_amd64.s](https://github.com/golang/go/blob/master/src/runtime/sys_linux_amd64.s), [runtime/netpoll_epoll.go](https://github.com/golang/go/blob/master/src/runtime/netpoll_epoll.go).
- [Таблица системных вызовов Linux x86_64](https://filippo.io/linux-syscall-table/), [список вызовов gVisor](https://gvisor.dev/docs/user_guide/compatibility/linux/amd64/).
- OSDev Wiki: [Getting to Ring 3](https://wiki.osdev.org/Getting_to_Ring_3), [SYSENTER](https://wiki.osdev.org/SYSENTER), [ELF](https://wiki.osdev.org/ELF), [SSE](https://wiki.osdev.org/SSE); [blog_os #970](https://github.com/phil-opp/blog_os/issues/970) о типичных ошибках пользовательского режима.
- [System V ABI для x86_64](https://gitlab.com/x86-psABIs/x86-64-ABI) — стартовый стек и auxv.
