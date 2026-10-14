---
description: Рецепты тестов OSel — тест на хосте в osel-core, тест в ядре, интеграционный тест, тест ожидаемого падения, запуск одного теста.
---

# Тесты: как добавить и запустить

Язык: [Русский](testing.md) · [English](../../../en/developer/guides/testing.md)

**Для кого:** для разработчиков и агентов, которые меняют код.

**Коротко:** логику без железа тестируйте в `osel-core` обычным `#[test]` (`cargo test-core`, без QEMU). То, что требует процессора и устройств, — `#[test_case]` в модуле ядра или отдельное тестовое ядро в `tests/`. Падения проверяйте макросом `osel::panic_test!`. Перед PR прогоните всё в обоих режимах загрузки. Как это устроено внутри — на странице [Как устроены тесты](../architecture/testing.md).

## Какой тест выбрать {#choose}

| Что проверяем | Вид теста | Где | Запуск |
|---|---|---|---|
| Логика без железа: аллокаторы, разбор, форматирование, консоль на буфере в памяти | тест на хосте | `osel-core/src/<модуль>.rs`, `#[cfg(test)] mod tests` | `cargo test-core` |
| Runner: разбор вывода, коды выхода | тест на хосте | `runner/src/main.rs` | `cargo test-runner` |
| Поведение ядра с настоящим процессором и устройствами | тест в ядре | `src/<модуль>.rs`, `#[test_case]` | `cargo test` |
| Сценарий, которому нужно своё ядро: другая инициализация, отдельный обработчик паники | интеграционный тест | `tests/<имя>.rs` | `cargo test` |
| Ядро должно упасть с определённым сообщением | тест падения | `tests/<имя>.rs`, `osel::panic_test!` | `cargo test` |

Если логику можно вынести в `osel-core`, выносите и тестируйте там: такие тесты идут доли секунды и дают точную ошибку.

## Тест на хосте в osel-core {#host}

1. В файле модуля в `osel-core/src/` найдите `#[cfg(test)] mod tests` или создайте его в конце файла.
2. Добавьте функцию с атрибутом `#[test]`. В тестах доступна стандартная библиотека (`std`), в остальном коде крейта — нет.
3. Запустите `cargo test-core`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_no_items() {
        let mut buffer = RingBuffer::<u8, 4>::new(0);
        assert_eq!(buffer.pop(), None);
    }
}
```

## Тест в ядре {#kernel}

1. В модуле ядра в `src/` добавьте функцию с атрибутом `#[test_case]`, лучше внутри `#[cfg(test)] mod tests`.
2. Тест работает в уже инициализированном ядре: куча, прерывания и консоль готовы.
3. Запустите `cargo test`. В выводе у теста строка вида `osel::<модуль>::tests::<имя>... [ok]`.

```rust
#[cfg(test)]
mod tests {
    #[test_case]
    fn test_timer_ticks() {
        let start = crate::interrupts::ticks();
        while crate::interrupts::ticks() < start + 2 {
            x86_64::instructions::hlt();
        }
    }
}
```

{% note warning %}

Каждый тест должен освободить всё, что выделил в куче, и вернуть все физические страницы. Иначе тест падает с сообщением `the test leaked N heap bytes` или `the test leaked N frames`. Таблицы страниц тоже страницы: снимайте отображения через `unmap_range`, он освобождает опустевшие таблицы сам; после голых `map_to` и `unmap` — `prune_empty_tables` по своему диапазону, см. [проверку утечек](../architecture/testing.md#leaks).

{% endnote %}

## Интеграционный тест {#integration}

Нужен, когда сценарию требуется своё ядро. Образец — `tests/heap_stress.rs`.

1. Создайте `tests/<имя>.rs` с собственной точкой входа (`entry_point!` с `osel::BOOTLOADER_CONFIG`), вызовом `osel::init`, `test_main()` и обработчиком паники `osel::testosterone::test_panic_handler`.
2. Пишите тесты как `#[test_case]`.
3. Регистрировать в `Cargo.toml` не нужно: cargo находит файлы в `tests/` сам. Регистрация нужна только тестам с `harness = false`, см. [тест падения](#panic-test).

Если тест не вызывает `osel::avganec::init`, у ядра нет консоли: `println!` ничего не делает, и это нормально.

## Тест падения {#panic-test}

Макрос `osel::panic_test!` из `src/testosterone.rs` создаёт тестовое ядро, которое инициализирует систему, выполняет тело и проходит, только если тело упало с паникой, а в её сообщении есть все ожидаемые части. Заодно он проверяет, что появился красный экран с заголовком.

```rust
#![no_std]
#![no_main]

osel::panic_test!("overflow::has_a_handler", ["EXCEPTION: OVERFLOW"], {
    unsafe { core::arch::asm!("int 4") };
});
```

Такой тест обязательно регистрируется в `Cargo.toml`, иначе cargo подставит стандартную обвязку тестов, и сборка упадёт:

```toml
[[test]]
name = "overflow"
harness = false
```

### Проверка точного адреса падения {#expect-rip}

Чтобы тест проверял не только название исключения, но и адрес инструкции, которая его вызвала, запишите этот адрес в `osel::testosterone::EXPECTED_RIP` прямо из ассемблера перед падающей инструкцией. Тогда `expect_panic` потребует в сообщении строку `rip: <адрес>`. Образец — `tests/invalid_opcode.rs`:

```rust
osel::panic_test!("invalid_opcode::is_reported", ["EXCEPTION: INVALID OPCODE"], {
    unsafe {
        core::arch::asm!(
            "lea {rip}, [rip + 2f]",
            "mov [{expected}], {rip}",
            "2: ud2",
            rip = out(reg) _,
            expected = in(reg) osel::testosterone::EXPECTED_RIP.as_ptr(),
        )
    };
});
```

Метка `2:` стоит на падающей инструкции, `lea` берёт её адрес.

## Как запустить {#run}

| Задача | Команда |
|---|---|
| Все тесты ядра, UEFI | `cargo test` |
| Все тесты ядра, BIOS | `OSEL_BOOT=bios cargo test` |
| Не останавливаться на первом упавшем тестовом ядре | `cargo test --no-fail-fast` |
| Только тесты из `src/` | `cargo test --lib` |
| Одно тестовое ядро из `tests/` | `cargo test --test overflow` |
| Дать тестам больше времени | `OSEL_TIMEOUT=120 cargo test` |

Отбор тестов по имени после `--` не работает, см. [справочник runner](../reference/runner.md). Сузить прогон можно только до одного тестового ядра через `--test` или `--lib`.

## Как проверить перед PR {#before-pr}

```shell
cargo test-core
cargo test-runner
cargo docs-check
cargo test --no-fail-fast
OSEL_BOOT=bios cargo test --no-fail-fast
OSEL_EXPECT="OSel ready to go!" cargo run
```

Последняя команда загружает настоящее ядро без окна и ждёт строку готовности. Всё это прогоняет и CI, но локальный прогон быстрее показывает ошибку.

## Частые ошибки {#errors}

| Что видно | Причина |
|---|---|
| Ошибка сборки про `test` и `#[panic_handler]` в новом файле из `tests/` | тест на `panic_test!` или с `harness = false` не добавлен в `Cargo.toml` |
| `the test leaked N heap bytes` | тест не освободил память, например сохранил `Box` в статической переменной или вызвал `mem::forget` |
| `the test leaked N frames` | тест не вернул физическую страницу или не освободил таблицы страниц, созданные под его отображения |
| `the test freed N frames it did not allocate` | тест вернул в пул страницы, которые выдавались до него |
| `N of M tests reported [ok]` | тестовое ядро завершилось успешно, но не все объявленные тесты отчитались, см. [Что пошло не так](troubleshooting.md#messages) |
| Тест проходит в UEFI и падает в BIOS (или наоборот) | разная раскладка памяти и устройств у прошивок; проверяйте в обоих режимах |
