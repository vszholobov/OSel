---
description: Завершённый этап 2.5 — дочерние процессы Go, execve, ожидание и внешняя clear.
---

# Дочерние программы из Go

**Для кого:** разработчик, который ищет результаты этапа 2.5.

**Коротко:** этап реализован в [PR №9](https://github.com/vszholobov/OSel/pull/9): Go запускает дочерние программы через `os/exec`, в том числе `/bin/clear`. Устройство — в [процессах](../architecture/core/threads.md#child-processes), [каналах](../architecture/core/fs.md#pipes), [справочнике вызовов](../reference/syscalls.md) и [решении 0017](../decisions/0017-child-processes.md). Вызов внешней очистки в [тетрисе](../architecture/tetris.md) сохранён.
