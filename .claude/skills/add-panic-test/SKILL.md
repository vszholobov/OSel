---
name: add-panic-test
description: Add an OSel integration test kernel that expects a panic or CPU exception (panic_test! macro), including the Cargo.toml registration and an exact rip check.
---

# Добавить тест падения

1. Файл `tests/<имя>.rs`:

   ```rust
   #![no_std]
   #![no_main]

   // What the test proves, in one line

   osel::panic_test!("<имя>::<что_проверяем>", ["EXCEPTION: <ИМЯ>", "<ещё фрагмент сообщения>"], {
       // код, который должен упасть
   });
   ```

   Тест проходит, только если сообщение паники содержит все фрагменты, а на экране красный экран с заголовком.

2. Регистрация в `Cargo.toml`:

   ```toml
   [[test]]
   name = "<имя>"
   harness = false
   ```

3. Точный адрес упавшей инструкции: в ассемблере записать адрес метки в `osel::testosterone::EXPECTED_RIP` прямо перед ней — образец в `tests/divide_error.rs` и `tests/invalid_opcode.rs`.

4. Проверить в обоих режимах: `OSEL_BOOT=uefi cargo test --test <имя>` и то же с `bios`.

5. Строка в `CHANGELOG.md` («Не выпущено»). Подробности о видах тестов — `docs/ru/developer/guides/testing.md`.
