---
description: Установка инструментов, запуск OSel в QEMU и прогон тестов на macOS и Linux.
---

# Быстрый старт

**Для кого:** для всех, кто хочет запустить OSel на своём компьютере в виртуальной машине.

**Коротко:** поставьте rustup и QEMU, склонируйте репозиторий и выполните `cargo run`. Откроется окно QEMU, в котором загрузится OSel. Всё остальное, включая нужную версию Rust, подтянется само.

## Что понадобится {#requirements}

- **git** и инструменты сборки (компоновщик и всё, что ему нужно).
- **rustup** — установщик Rust. Нужную версию компилятора (nightly с закреплённой датой) он поставит сам: она записана в файле `rust-toolchain.toml`.
- **QEMU** — эмулятор компьютера, в котором запускается OSel.
- **Go 1.27** — для сборки тетриса и тестовых Go-программ. Без Go запускается только ядро с программами на Rust.

{% list tabs %}

- macOS

  1. Поставьте инструменты разработчика Apple (в них git и компоновщик) и [Homebrew](https://brew.sh), если их ещё нет:

     ```shell
     xcode-select --install
     /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
     ```

  2. Поставьте rustup и QEMU:

     ```shell
     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
     brew install qemu
     ```

- Linux (Debian, Ubuntu)

  ```shell
  sudo apt update && sudo apt install build-essential git curl qemu-system-x86
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

{% endlist %}

Установщик rustup задаст вопрос о настройках: нажмите Enter, чтобы согласиться со стандартными. После установки откройте новое окно терминала или выполните `. "$HOME/.cargo/env"`, иначе команда `cargo` не найдётся.

## Запуск {#run}

```shell
git clone --recurse-submodules https://github.com/vszholobov/OSel
cd OSel
cargo run
```

Первый запуск долгий: rustup скачивает компилятор, cargo собирает загрузчик, а для режима UEFI скачивается прошивка OVMF (UEFI для QEMU). Следующие запуски занимают секунды.

В окне QEMU появятся строки `OSel 0.1.0 (commit …) woke up!`, `Hello from ring 3!` (это первая программа из ramdisk) и `OSel ready to go!`. При установленном Go затем начинается одиночный тетрис: `a`/`d` — движение, `s` — вниз, `q`/`e` — поворот, `x` — выход. После игры или без Go ядро печатает на экране набираемые символы. Уже склонированный репозиторий обновите командой `git submodule update --init --recursive`.

Чтобы выйти, закройте окно QEMU или нажмите Ctrl+C в терминале, где запущен `cargo run`.

По умолчанию OSel загружается так, как загружаются современные компьютеры, через UEFI. Чтобы проверить старый режим BIOS:

```shell
OSEL_BOOT=bios cargo run
```

## Тесты {#tests}

| Команда | Что проверяет | Нужен QEMU |
|---|---|---|
| `cargo test-core` | логику ядра, которая не зависит от железа | нет |
| `cargo test-runner` | программу, которая запускает QEMU | нет |
| `cargo test` | ядро целиком: каждое тестовое ядро загружается в QEMU | да |
| `OSEL_BOOT=bios cargo test` | то же в режиме BIOS | да |

Подробнее — в рецептах [про тесты](../guides/testing.md).

## Где лежат образы дисков {#images}

`cargo run` собирает из ядра образ диска и только потом запускает QEMU:

- `target/x86_64-unknown-none/debug/OSel.uefi.img` — для загрузки через UEFI;
- `target/x86_64-unknown-none/debug/OSel.bios.img` — после `OSEL_BOOT=bios cargo run`.

Эти образы можно записать на флешку и загрузить OSel на настоящем компьютере: см. [Запуск с флешки](usb.md).

## Частые ошибки {#errors}

| Что видно | Что делать |
|---|---|
| `qemu-system-x86_64: command not found` | QEMU не установлен, см. [Что понадобится](#requirements) |
| `cargo: command not found` сразу после установки rustup | откройте новое окно терминала или выполните `. "$HOME/.cargo/env"` |
| `linker 'cc' not found` | не стоят инструменты сборки: `xcode-select --install` на macOS, `build-essential` на Linux |
| Ошибка скачивания OVMF при первом запуске | нужен интернет; прошивка кэшируется в `runner/.ovmf`, повторите запуск |
| Красный экран «OSel crashed :(» | ядро упало, см. [Красный экран](crash-report.md) |

Другие ситуации — в разделе [Что пошло не так](../guides/troubleshooting.md).
