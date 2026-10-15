# OSel — инструкции для ИИ-агентов

OSel — учебная операционная система на Rust для x86_64: ядро на `bootloader` 0.11 (UEFI и BIOS), разработка и тесты в QEMU. Цель — запустить консольный тетрис на Go через системные вызовы Linux.

Вся документация — на сайте из папки `docs/`, точки входа `docs/ru/index.md` и `docs/en/index.md`; разделы `user/` и `developer/` в каждой локали. Этот файл говорит, что читать; сам он знания не пересказывает.

## Команды

```sh
cargo test-core                     # unit-тесты osel-core на хосте, без QEMU, доли секунды
cargo test-runner                   # тесты runner на хосте
OSEL_BOOT=uefi cargo test           # все тесты ядра в QEMU, загрузка через UEFI
OSEL_BOOT=bios cargo test           # то же через BIOS
cargo test --test <имя>             # одно интеграционное ядро из tests/ (фильтр после -- не работает, см. docs/ru/developer/reference/runner.md)
cargo docs-check                    # документация совпадает с кодом
OSEL_EXPECT="OSel ready to go!" cargo run   # запуск настоящего ядра без окна, ждёт строку в последовательном порту
```

Сайт документации собирает только CI, локально его собирать не нужно.

## Задача → что читать

| Задача | Страницы |
|---|---|
| Добавить обработчик исключения | `docs/ru/developer/architecture/core/interrupts.md`, `docs/ru/developer/reference/vectors.md`, `docs/ru/developer/guides/testing.md` |
| Добавить тест (хост, ядро, интеграционный, тест падения) | `docs/ru/developer/guides/testing.md`, `docs/ru/developer/architecture/testing.md` |
| Добавить системный вызов (этап 2) | `docs/ru/developer/reference/syscalls.md`, `docs/ru/developer/architecture/core/user-mode.md`, `docs/ru/developer/design/stage-2-user-mode.md` |
| Добавить переменную окружения runner | `docs/ru/developer/reference/runner.md`, `runner/src/main.rs` |
| Обновить тулчейн Rust | `docs/ru/developer/guides/toolchain-and-ci.md`, `docs/ru/developer/decisions/0004-pinned-nightly.md` |
| Упал CI или тест | `docs/ru/developer/guides/troubleshooting.md`, `docs/ru/developer/guides/debugging.md` |
| Поменять память, прерывания или блокировки | `docs/ru/developer/architecture/core/memory.md`, `docs/ru/developer/architecture/core/interrupts.md`, `docs/ru/developer/architecture/core/locking.md` |
| Читать файлы в ядре, положить файл в ramdisk, подключить диск или файловую систему | `docs/ru/developer/architecture/core/fs.md`, `docs/ru/developer/decisions/0007-storage-interfaces.md` |
| Поменять консоль, порт, клавиатуру | `docs/ru/developer/architecture/drivers/console.md`, `docs/ru/developer/architecture/drivers/serial.md`, `docs/ru/developer/architecture/drivers/keyboard.md` |
| Запустить ядро, снять скриншот | навык `run-kernel` (`.claude/skills/run-kernel/SKILL.md`), `docs/ru/developer/guides/debugging.md` |
| Разобрать падение или зависание | навык `debug-qemu-crash`, `docs/ru/developer/guides/debugging.md` |
| Писать документацию | `docs/ru/developer/guides/writing-docs.md` |
| Понять, почему сделано так | `docs/ru/developer/decisions/index.md` |

## Перед задачей этапа N

1. Раздел этапа в `docs/ru/developer/roadmap.md`: якоря `#stage-1` … `#stage-4`.
2. Страница этапа в `docs/ru/developer/design/` (для этапа 2 — `docs/ru/developer/design/stage-2-user-mode.md`).
3. `docs/ru/developer/architecture/overview.md` — карта модулей.
4. Страницы подсистем, которые задача трогает, вместе с их разделами «Грабли».
5. Связанные решения в `docs/ru/developer/decisions/`.

## Определение готовности

- Тест на нужном уровне: чистая логика — в `osel-core` (`cargo test-core`), железо — тест ядра или интеграционное ядро в `tests/`. Интеграционное ядро с `harness = false` регистрируется в `Cargo.toml` как `[[test]]`.
- Проходят `cargo test-core`, `cargo test-runner`, `cargo test` в обоих режимах загрузки, тест не оставляет утечек кучи.
- Документация обновлена по таблице «куда класть знание» из `docs/ru/developer/guides/writing-docs.md`; страница и перевод обновлены, новая страница добавлена в `docs/ru/toc.yaml` и `docs/en/toc.yaml`; проходит `cargo docs-check`.
- В `CHANGELOG.md` в разделе «Не выпущено» есть строка об изменении. Это касается каждого PR, в том числе только документации или тестов.
- Запуск настоящего ядра проходит: `OSEL_EXPECT="OSel ready to go!" cargo run`.

## Ветки и PR

- Базовая ветка — `master`. На каждую тему — своя ветка от свежего `master`.
- PR открывается по шаблону `.github/pull_request_template.md`, CI должен быть зелёным.
- Все PR и релизы оформляются на английском: заголовки, описания и комментарии в PR, названия и описания релизов.
- Коммиты и описания PR — без `Co-Authored-By` и без упоминаний ИИ.

## Имена модулей

У существующих модулей имена-каламбуры владельца: `avganec` (консоль), `cereal` (последовательный порт), `testosterone` (тестовый фреймворк), `qemuno` (выход из QEMU). Их не переименовывать. Новому модулю — понятное английское имя (`syscall`, `scheduler`, `elf`); каламбур — только если его предложил владелец. Каждый новый модуль попадает в карту модулей `docs/ru/developer/architecture/overview.md`.

## Правила проекта

- Никогда не добавлять в коммиты и описания PR `Co-Authored-By` и любые другие указания на Claude или ИИ.
- Один PR на этап или на одну тему.
- Не копировать сторонний код с несовместимой лицензией.
- Документация — на русском и английском в `docs/ru/` и `docs/en/`; комментарии в коде и сообщения коммитов — на английском.
- Не устанавливать программы на машину владельца без спроса.
