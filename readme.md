<p align="center"><img src="docs/images/osel-logo.svg" width="160" alt="Логотип OSel: осёл в визоре с командной строкой"></p>

# OSel

Учебная операционная система на Rust для x86_64: загружается через UEFI и BIOS, работает в QEMU и на ноутбуках с флешки. Цель — запустить на ней консольный тетрис на Go.

**Документация:** [Русский](https://vszholobov.github.io/OSel/ru/) · [English](https://vszholobov.github.io/OSel/en/). [Пользователю](docs/ru/user/index.md) — готовые образы и игра; [разработчику](docs/ru/developer/index.md) — сборка, тесты и устройство ядра.

```shell
cargo run          # собрать и запустить в QEMU
cargo test         # тесты ядра в QEMU
cargo test-core    # тесты логики ядра на хосте, без QEMU
```

С чего начать: [быстрый старт](docs/ru/developer/guides/quickstart.md). ИИ-агентам: [`AGENTS.md`](AGENTS.md). История изменений: [`CHANGELOG.md`](CHANGELOG.md).
