---
description: Установка инструментов, запуск OSel в QEMU и прогон тестов на macOS и Linux.
---

# Быстрый старт

**Для кого:** для всех, кто хочет запустить OSel на своём компьютере в виртуальной машине.

**Коротко:** поставьте rustup и QEMU, склонируйте репозиторий и выполните `cargo run`. Откроется окно QEMU, в котором загрузится OSel. Всё остальное, включая нужную версию Rust, подтянется само.

## Что понадобится {#requirements}

- **rustup** — установщик Rust. Нужную версию компилятора (nightly с закреплённой датой) он поставит сам: она записана в файле `rust-toolchain.toml`.
- **QEMU** — эмулятор компьютера, в котором запускается OSel.
- **git**.

{% list tabs %}

- macOS

  ```shell
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  brew install qemu
  ```

- Linux (Debian, Ubuntu)

  ```shell
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  sudo apt update && sudo apt install qemu-system-x86
  ```

{% endlist %}

## Запуск {#run}

```shell
git clone https://github.com/vszholobov/OSel
cd OSel
cargo run
```

Первый запуск долгий: rustup скачивает компилятор, cargo собирает загрузчик, а для режима UEFI скачивается прошивка OVMF (UEFI для QEMU). Следующие запуски занимают секунды.

В окне QEMU появятся строки `OSel woke up!` и `OSel ready to go!`. После этого OSel печатает на экране всё, что вы набираете на клавиатуре.

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
| Ошибка скачивания OVMF при первом запуске | нужен интернет; прошивка кэшируется в `runner/.ovmf`, повторите запуск |
| Красный экран «OSel crashed :(» | ядро упало, см. [Красный экран](crash-report.md) |

Другие ситуации — в разделе [Что пошло не так](../guides/troubleshooting.md).
