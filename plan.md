# План развития OSel

Цель: запустить на OSel консольный [тетрис на Go](https://github.com/vszholobov/Tetris), сначала в QEMU, потом на ноутбуке с флешки. Сетевая игра — по желанию, отдельным этапом.

Основной подход: Go-программа собирается как обычная программа для Linux
(`GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build`), а ядро понимает системные вызовы Linux
в объёме, нужном рантайму Go и тетрису.

Разработка и отладка идут в QEMU (`cargo run`, `cargo test`, CI на GitHub). Ноутбук включается на контрольных точках.

## Уже сделано

- Ядро по главам 1–6 блога: загрузка, вывод на экран, тесты, исключения процессора, double fault.
- Свежий nightly, `bootloader 0.11`, загрузка через UEFI и BIOS.
- `avganec` выводит текст через framebuffer, работает и на современных компьютерах без VGA.
- `runner/` собирает образ диска и запускает QEMU, тесты гоняются в CI в обоих режимах загрузки.

## Этап 1. Доделать ядро

- [x] Аппаратные прерывания: таймер, клавиатура PS/2, `hlt` вместо пустого цикла.
- [x] Страничная память: таблицы страниц, выделение физических страниц. Ядро и всё, что размещает загрузчик, живёт в верхней половине адресов, нижняя свободна под программы.
- [x] Куча и аллокатор, крейт `alloc` (`Vec`, `Box`, `String`).
- [x] Понятный экран паники: текст ошибки, адрес, регистры. На ноутбуке другого способа отладки не будет.
- [ ] Скрипт записи образа на флешку с проверкой, что выбран именно USB-диск. Отложено до проверки на ноутбуке.
- [x] Сверка с блогом и курсом ШАДа: блокировки, которые сами выключают прерывания и сообщают о взаимоблокировке
  (как `IrqSpinlock` в Nikka), разделение отображений ядра и программ, отдельные стеки для NMI и machine check,
  проверка утечек кучи после каждого теста, тесты из лаб 1-hw-4, 2-mm-1, 2-mm-4, 2-mm-5.

**Проверка на ноутбуке:** ядро загружается на Samsung с флешки и печатает нажатые клавиши.

Где смотреть:
- Блог, главы 7–11: [Hardware Interrupts](https://os.phil-opp.com/hardware-interrupts/),
  [Introduction to Paging](https://os.phil-opp.com/paging-introduction/),
  [Paging Implementation](https://os.phil-opp.com/paging-implementation/),
  [Heap Allocation](https://os.phil-opp.com/heap-allocation/),
  [Allocator Designs](https://os.phil-opp.com/allocator-designs/).
  Блог написан под `bootloader 0.9`: код получения карты памяти и `BootInfo` придётся адаптировать под 0.11.
- Nikka, лабы 1-hw (часы, таймеры, прерывания), 2-mm (память), 3-allocator (аллокаторы):
  [описание лаб](https://sergey-v-galtsev.gitlab.io/labs-description/lab/book/index.html),
  [репозиторий](https://gitlab.com/sergey-v-galtsev/nikka-public). Стек тот же (`bootloader 0.11`, `x86_64-unknown-none`).
- [Миграция на bootloader 0.11](https://github.com/rust-osdev/bootloader/tree/main/docs/migration).
- OSDev Wiki: [8259 PIC](https://wiki.osdev.org/8259_PIC), [PS/2 Keyboard](https://wiki.osdev.org/PS/2_Keyboard),
  [Paging](https://wiki.osdev.org/Paging).

## Этап 2. Запуск программ

- [ ] Пользовательский режим: сегменты кольца 3 в GDT, стек ядра в TSS, вход через `syscall`/`sysret`.
- [ ] Загрузка ELF из ramdisk (`DiskImageBuilder::set_ramdisk` в `runner/`), стартовый стек с argv, envp и auxv.
- [ ] Системные вызовы `write` и `exit`.
  **Проверка:** программа на ассемблере печатает «hello».
- [ ] SSE для пользовательских программ: включение и сохранение регистров при переключении (`fxsave`/`xsave`).
- [ ] Потоки и планировщик с вытеснением по таймеру, `clone`, `futex`.
- [ ] Системные вызовы Linux для рантайма Go (около 30): `mmap`/`munmap`/`madvise`, `arch_prctl`,
  `rt_sigaction`/`rt_sigprocmask`/`sigaltstack`, `clock_gettime`, `nanosleep`, `sched_yield`,
  `getrandom`, `gettid`, `tgkill`, `exit_group` и другие. Неподдержанные пока возвращают `ENOSYS`.
  **Проверка:** Go-программа «hello world».
- [ ] Таймеры и упрощённое ожидание событий (`epoll` только для сна с таймаутом и пробуждения).
  **Проверка:** Go-программа с горутинами и `time.Sleep`.

Где смотреть:
- Nikka, лабы 4-process (ELF, пользовательский режим, системные вызовы, планировщик, вытеснение) и 5-sync.
- Ядра на Rust, совместимые с Linux:
  [Kerla](https://github.com/nuta/kerla) (небольшое, хорошо читается),
  [Asterinas](https://github.com/asterinas/asterinas),
  [Maestro](https://github.com/maestro-os/maestro).
- Какие системные вызовы зовёт Go: [runtime/os_linux.go](https://github.com/golang/go/blob/master/src/runtime/os_linux.go),
  [runtime/sys_linux_amd64.s](https://github.com/golang/go/blob/master/src/runtime/sys_linux_amd64.s).
- [Таблица системных вызовов Linux x86_64](https://filippo.io/linux-syscall-table/),
  [man-страницы](https://man7.org/linux/man-pages/dir_section_2.html),
  [список поддержанных вызовов в gVisor](https://gvisor.dev/docs/user_guide/compatibility/linux/amd64/) —
  удобно смотреть, без чего программы обычно обходятся.
- OSDev Wiki: [Getting to Ring 3](https://wiki.osdev.org/Getting_to_Ring_3),
  [SYSCALL/SYSENTER](https://wiki.osdev.org/SYSENTER), [ELF](https://wiki.osdev.org/ELF), [SSE](https://wiki.osdev.org/SSE).
- [System V ABI для x86_64](https://gitlab.com/x86-psABIs/x86-64-ABI) — раздел про стартовый стек процесса и auxv.

## Этап 3. Тетрис

В репозитории тетриса:
- [ ] Одиночный режим: перенести логику поля из `server/field` в общий `lib/`, игровой цикл (`processGameField`, `inputControl`) — в клиент.
- [ ] Очистка экрана последовательностью `\033[H\033[2J` вместо запуска программы `clear`.

В ядре:
- [ ] `/dev/tty`: открытие по пути, режимы терминала через `ioctl` (`TCGETS`/`TCSETS`/`TIOCGWINSZ`), ввод через последовательный порт.
- [ ] Тетрис в ramdisk, запуск при старте ядра.
  **Проверка:** тетрис играется в QEMU.
- [ ] Автотест в CI: подать нажатия через последовательный порт и проверить вывод.
- [ ] Для реального экрана: в `avganec` поддержать управляющие последовательности
  (курсор вверх/вниз/вправо, `\r`, скрыть и показать курсор, очистка экрана) и шрифт с кириллицей (фигуры рисуются буквой «Ж»).
- [ ] Ввод с клавиатуры ноутбука в `/dev/tty`.

**Проверка на ноутбуке:** тетрис на Samsung с флешки.

Где смотреть:
- [Управляющие последовательности ANSI](https://en.wikipedia.org/wiki/ANSI_escape_code), [справочник VT100](https://vt100.net/docs/vt100-ug/chapter3.html).
- [Формат шрифтов PSF](https://wiki.osdev.org/PC_Screen_Font): в нём консольные шрифты Linux, есть с кириллицей (например, Terminus).
- Библиотека терминала, которую использует тетрис: [mattn/go-tty](https://github.com/mattn/go-tty) (`tty_unix.go`).
- Поведение терминала в Linux: [termios(3)](https://man7.org/linux/man-pages/man3/termios.3.html), [ioctl_tty(2)](https://man7.org/linux/man-pages/man2/ioctl_tty.2.html).

## Этап 4 (по желанию). Сеть и современное железо

- [ ] Перебор устройств на шине PCI, поиск сетевой карты.
- [ ] Драйвер сетевой карты: сначала Intel `e1000e` в QEMU, потом карта ноутбука. Прерывания можно заменить опросом по таймеру.
- [ ] Сетевой стек на [smoltcp](https://github.com/smoltcp-rs/smoltcp): DHCP, TCP.
- [ ] Сокеты (`socket`, `connect`, `read`/`write`, `setsockopt`/`getsockopt`, `getsockname`/`getpeername`) и полноценный `epoll`.
- [ ] `/etc/hosts` в ramdisk с адресом сервера тетриса, чтобы не делать DNS и UDP.
  **Проверка:** сетевой тетрис против настоящего сервера.
- [ ] Запуск на Acer Nitro через UEFI.

Где смотреть:
- OSDev Wiki: [PCI](https://wiki.osdev.org/PCI), [Intel Ethernet i217](https://wiki.osdev.org/Intel_Ethernet_i217),
  [RTL8169](https://wiki.osdev.org/RTL8169), [APIC](https://wiki.osdev.org/APIC).
- Драйверы сетевых карт на Rust в [Redox OS](https://gitlab.redox-os.org/redox-os/base/-/tree/main/drivers/net) (`e1000d`, `rtl8168d`, `virtio-netd`).
- Nikka, лабы 1-hw-7 (PCI) и 3-smp (многоядерность, APIC).

## Железо

| | Samsung NP530U4B | Acer Nitro |
|---|---|---|
| Режим загрузки | BIOS (`OSel.bios.img`) | UEFI (`OSel.uefi.img`) |
| Подготовка BIOS | F2 → Advanced → Fast BIOS Mode → Disabled | F2 → Security → задать Supervisor Password → Boot → Secure Boot → Disabled, Main → F12 Boot Menu → Enabled |
| Меню загрузки | F10 | F12 |
| Риски | Безопаснее грузиться в BIOS-режиме: в UEFI Samsung тех лет мог перестать включаться после записи настроек | Если включён BitLocker, перед выключением Secure Boot сохранить ключ восстановления |

Общее:
- Ядро не трогает диски ноутбука: драйверов дисков нет. При зависании держать кнопку питания, вынуть флешку — загрузится Windows.
- Проверить сетевую карту: Диспетчер устройств → Сетевые адаптеры. Intel удобнее всего, его изображает QEMU.
- Чтобы QEMU был похож на ноутбук: `cargo run -- -cpu SandyBridge -m 4G` (Samsung),
  `cargo run -- -machine q35 -cpu Skylake-Client -m 8G` (Nitro).
- Позже, для меню выбора между Windows и OSel на одном диске: [Limine](https://github.com/limine-bootloader/limine).
