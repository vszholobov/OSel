---
name: debug-qemu-crash
description: Investigate an OSel crash, hang, reboot loop or failing kernel test in QEMU — panic screen, serial output, QEMU interrupt logs, debugger attach, screenshots.
---

# Разобрать падение в QEMU

1. Прочитать вывод последовательного порта: экран паники дублируется туда строкой `OSel <версия> (commit <хеш>) crashed: …`. Сообщение «deadlock on lock `X`: held since …, waiting at …» — взаимоблокировка, см. `docs/ru/developer/architecture/core/locking.md`.
2. Сверить симптом с `docs/ru/developer/guides/troubleshooting.md`.
3. Перезагрузка по кругу или QEMU молча выходит с кодом 0 — тройной сбой. Запустить без окна, с журналом исключений в файл и с пределом по времени, чтобы QEMU не повис (runner сам добавляет `-no-reboot`):

   ```sh
   OSEL_EXPECT="OSel ready to go!" OSEL_TIMEOUT=60 cargo run -- -d int,cpu_reset -D /tmp/qemu-int.log
   tail -50 /tmp/qemu-int.log
   ```

   Для тестового ядра то же даёт `cargo test --test <имя>`: у тестов свой таймаут, но аргументы после `--` им не передаются, журнал исключений там недоступен.

4. Отладчик — строго по `docs/ru/developer/guides/debugging.md`, раздел «Пошаговая отладка»: `cargo run -- -s -S`, затем lldb с загрузкой символов со сдвигом `target modules load --file OSel --slide 0xffff800000000000` и `gdb-remote 1234`. Без сдвига точки останова не сработают. Здесь QEMU открывает окно и ждёт отладчик: закрыть его по окончании.
5. Посмотреть экран — скриншот через монитор QEMU, рецепт в `docs/ru/developer/guides/debugging.md`, раздел «Скриншот экрана».
6. Один тест отдельно: `cargo test --test <имя>`; таймаут теста меняет `OSEL_TIMEOUT=<секунды>`.

Не оставлять работающий QEMU: проверить `pgrep -fl qemu-system`.
