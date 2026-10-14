---
paths:
  - "osel-core/**"
---

# osel-core

- Крейт собирается на любом хосте: никаких портов, ассемблера и регистров процессора, `x86_64` без функции `instructions` (`docs/ru/developer/decisions/0002-osel-core-host-tests.md`).
- Каждое изменение — с unit-тестом, проверка — `cargo test-core`. Крайние случаи и случайные нагрузки здесь дешёвые, их и стоит проверять.
- Новый модуль — в карту модулей `docs/ru/developer/architecture/overview.md` и на страницу подсистемы, которая его использует.
