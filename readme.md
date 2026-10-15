<p align="center"><img src="docs/images/osel-logo.svg" width="160" alt="OSel logo: a donkey wearing a command-line visor"></p>

# OSel

OSel — вайбовая операционная система.

OSel — a vibey operating system.

**Русский:** [документация](https://vszholobov.github.io/OSel/ru/) · [пользователю](docs/ru/user/index.md) · [разработчику](docs/ru/developer/index.md) · [быстрый старт](docs/ru/developer/guides/quickstart.md).

**English:** [documentation](https://vszholobov.github.io/OSel/en/) · [user guide](docs/en/user/index.md) · [developer guide](docs/en/developer/index.md) · [quick start](docs/en/developer/guides/quickstart.md).

[Скачать / Downloads](https://github.com/vszholobov/OSel/releases/latest) · [История изменений / Changelog](CHANGELOG.md) · [Правила / Contributing](AGENTS.md).

Код / Code: [ядро / kernel](src/) · [драйверы / drivers](src/drivers/) · [логика / core logic](osel-core/) · [runner](runner/).

```shell
cargo run          # build and run in QEMU
cargo test         # kernel tests in QEMU
cargo test-core    # core logic tests on the host
```

Все PR и релизы — на английском. All pull requests and releases must be written in English.
