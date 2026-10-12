---
paths:
  - "src/**"
  - "tests/**"
---

# Ядро OSel

- Сначала прочитать `docs/architecture/overview.md` и страницу подсистемы в `docs/architecture/`, включая раздел «Грабли». Для работы этапа 2 (пользовательский режим, системные вызовы, ELF, потоки) — ещё `docs/design/stage-2-user-mode.md`.
- Чистую логику писать в `osel-core/` с тестами на хосте, в ядре — только железо, порты и глобальные объекты.
- Каждый глобальный объект — под `IrqSpinlock` (`src/sync.rs`) с именем. Из обработчиков исключений не брать блокировки, которые может держать прерванный код; печатать через `avganec::try_print`.
- Не ломать раскладку адресов: ядро и куча — в верхней половине, нижняя пуста (`docs/decisions/0006-higher-half-kernel.md`).
- Интеграционное ядро в `tests/` с `harness = false` — запись `[[test]]` в `Cargo.toml`; тест падения — через `osel::panic_test!` (`docs/guides/testing.md`).
- После изменения обновить страницу подсистемы, `docs/reference/vectors.md` для новых векторов, `docs/architecture/core/locking.md` для новых блокировок, `docs/architecture/overview.md` для новых модулей.
