---
name: debug-qemu-crash
description: Investigate an OSel crash, hang, reboot loop or failing kernel test in QEMU — panic screen, serial output, QEMU interrupt logs, debugger attach, screenshots.
---

# Разобрать падение в QEMU

1. Прочитать вывод последовательного порта: экран паники дублируется туда («OSel crashed: …»). Сообщение «deadlock on lock `X`: held since …, waiting at …» — взаимоблокировка, см. `docs/architecture/core/locking.md`.
2. Сверить симптом с `docs/guides/troubleshooting.md`.
3. Перезагрузка по кругу или QEMU молча выходит с кодом 0 — тройной сбой. Запустить с журналом исключений:

   ```sh
   cargo run -- -d int,cpu_reset -no-reboot 2>&1 | tail -50
   ```

4. Отладчик: `cargo run -- -s -S`, затем `lldb target/x86_64-unknown-none/debug/OSel` и `gdb-remote 1234`.
5. Посмотреть экран: скриншот через монитор QEMU, как в навыке `run-kernel`.
6. Один тест отдельно: `cargo test --test <имя>`; таймаут теста меняет `OSEL_TIMEOUT=<секунды>`.

Не оставлять работающий QEMU: проверить `pgrep -fl qemu-system`. Подробности — `docs/guides/debugging.md`.
