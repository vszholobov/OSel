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

Скриншот экрана: запустить QEMU с монитором на сокете, затем через монитор сделать `screendump` и `quit`:

```sh
cargo run -- -display none -monitor unix:/tmp/osel-mon.sock,server,nowait &
# подождать загрузки, затем:
printf 'screendump /tmp/osel.png -f png\nquit\n' | nc -U /tmp/osel-mon.sock
```

Через монитор можно и нажимать клавиши: `sendkey h`, `sendkey shift-1`.

Не оставлять работающий QEMU: проверить `pgrep -fl qemu-system`. Подробности — `docs/guides/debugging.md`, переменные runner — `docs/reference/runner.md`.
