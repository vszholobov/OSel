---
paths:
  - "src/**"
  - "tests/**"
---

# Ядро OSel

- Сначала прочитать `docs/ru/developer/architecture/overview.md` и страницу подсистемы в `docs/ru/developer/architecture/`, включая раздел «Грабли». Для работы этапа 2 (пользовательский режим, системные вызовы, ELF, потоки) — ещё `docs/ru/developer/design/stage-2-user-mode.md`.
- Чистую логику писать в `osel-core/` с тестами на хосте, в ядре — только железо, порты и глобальные объекты.
- Каждый глобальный объект — под `IrqSpinlock` (`src/sync.rs`) с именем. Из обработчиков исключений не брать блокировки, которые может держать прерванный код; печатать через `avganec::try_print`.
- Не ломать раскладку адресов: ядро и куча — в верхней половине; нижняя половина таблицы ядра пуста, у программ она своя в `UserSpace` (`docs/ru/developer/decisions/0006-higher-half-kernel.md`).
- Интеграционное ядро в `tests/` с `harness = false` — запись `[[test]]` в `Cargo.toml`; тест падения — через `osel::panic_test!` (`docs/ru/developer/guides/testing.md`).
- После изменения обновить страницу подсистемы, `docs/ru/developer/reference/vectors.md` для новых векторов, `docs/ru/developer/architecture/core/locking.md` для новых блокировок, `docs/ru/developer/architecture/overview.md` для новых модулей.
