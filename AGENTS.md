# OSel — инструкции для ИИ-агентов

OSel — учебная операционная система на Rust для x86_64: ядро на `bootloader` 0.11 (UEFI и BIOS), разработка и тесты в QEMU. Цель — запустить консольный тетрис на Go через системные вызовы Linux.

Вся документация — на сайте из папки `docs/`, точка входа `docs/index.md`. Этот файл говорит, что читать; сам он знания не пересказывает.

## Команды

```sh
cargo test-core                     # unit-тесты osel-core на хосте, без QEMU, доли секунды
cargo test-runner                   # тесты runner на хосте
OSEL_BOOT=uefi cargo test           # все тесты ядра в QEMU, загрузка через UEFI
OSEL_BOOT=bios cargo test           # то же через BIOS
cargo test --test <имя>             # одно интеграционное ядро из tests/; `cargo test -- <фильтр>` не работает
cargo docs-check                    # документация совпадает с кодом
OSEL_EXPECT="OSel ready to go!" cargo run   # запуск настоящего ядра без окна, ждёт строку в последовательном порту
```

Сайт документации собирает только CI, локально его собирать не нужно.

## Задача → что читать

| Задача | Страницы |
|---|---|
| Добавить обработчик исключения | `docs/architecture/core/interrupts.md`, `docs/reference/vectors.md`, `docs/guides/testing.md` |
| Добавить тест (хост, ядро, интеграционный, тест падения) | `docs/guides/testing.md`, `docs/architecture/testing.md` |
| Добавить системный вызов (этап 2) | `docs/design/stage-2-user-mode.md` |
| Добавить переменную окружения runner | `docs/reference/runner.md`, `runner/src/main.rs` |
| Обновить тулчейн Rust | `docs/guides/toolchain-and-ci.md`, `docs/decisions/0004-pinned-nightly.md` |
| Перенести тест из курса ШАДа | `docs/nikka.md` |
| Упал CI или тест | `docs/guides/troubleshooting.md`, `docs/guides/debugging.md` |
| Поменять память, прерывания или блокировки | `docs/architecture/core/memory.md`, `docs/architecture/core/interrupts.md`, `docs/architecture/core/locking.md` |
| Поменять консоль, порт, клавиатуру | `docs/architecture/drivers/` |
| Писать документацию | `docs/guides/writing-docs.md` |
| Понять, почему сделано так | `docs/decisions/index.md` |

## Перед задачей этапа N

1. Раздел этапа в `docs/roadmap.md`.
2. Страница этапа в `docs/design/` (для этапа 2 — `docs/design/stage-2-user-mode.md`).
3. `docs/architecture/overview.md` — карта модулей.
4. Страницы подсистем, которые задача трогает, вместе с их разделами «Грабли».
5. Связанные решения в `docs/decisions/`.

## Определение готовности

- Тест на нужном уровне: чистая логика — в `osel-core` (`cargo test-core`), железо — тест ядра или интеграционное ядро в `tests/`. Интеграционное ядро с `harness = false` регистрируется в `Cargo.toml` как `[[test]]`.
- Проходят `cargo test-core`, `cargo test-runner`, `cargo test` в обоих режимах загрузки, тест не оставляет утечек кучи.
- Документация обновлена по таблице «куда класть знание» из `docs/guides/writing-docs.md`; новая страница добавлена в `docs/toc.yaml`; проходит `cargo docs-check`.
- В `CHANGELOG.md` в разделе «Не выпущено» есть строка об изменении.

## Правила проекта

- Никогда не добавлять в коммиты и описания PR `Co-Authored-By` и любые другие указания на Claude или ИИ.
- Один PR на этап или на одну тему.
- Не копировать код из курса ШАДа Nikka: он под AGPL-3.0. Только идеи и замысел тестов, см. `docs/nikka.md`.
- Текст для людей (документация, сообщения в PR) — на русском; комментарии в коде и сообщения коммитов — на английском.
- Не устанавливать программы на машину владельца без спроса.
