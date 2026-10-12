---
name: run-kernel
description: Run OSel tests in both boot modes (UEFI and BIOS), boot the real kernel headless, or take a screenshot of the QEMU screen. Use after changing kernel code or to see what the kernel draws.
---

# Прогнать ядро

Тесты в обоих режимах загрузки, все тестовые ядра даже после первого падения:

```sh
OSEL_BOOT=uefi cargo test --no-fail-fast
OSEL_BOOT=bios cargo test --no-fail-fast
```

Перед ними быстрые тесты на хосте: `cargo test-core` и `cargo test-runner`.

Настоящее ядро без окна, успех — когда строка появилась в последовательном порту:

```sh
OSEL_EXPECT="OSel ready to go!" cargo run
```

Скриншот экрана и нажатия клавиш через монитор QEMU — рецепт в `docs/guides/debugging.md`, раздел «Скриншот экрана». Запускать там QEMU только без окна (`-display none`) и закрывать командой `quit` монитора.

Не оставлять работающий QEMU: проверить `pgrep -fl qemu-system`. Подробности — `docs/guides/debugging.md`, переменные runner — `docs/reference/runner.md`.
