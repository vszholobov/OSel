---
description: Закреплённый nightly-тулчейн и порядок его обновления, задачи CI на GitHub Actions и что каждая проверяет.
---

# Тулчейн и CI

Язык: [Русский](toolchain-and-ci.md) · [English](../../../en/developer/guides/toolchain-and-ci.md)

**Для кого:** для разработчиков и агентов, которые обновляют компилятор или разбираются с CI.

**Коротко:** версия nightly закреплена в `rust-toolchain.toml`, и обновляют её руками, только когда еженедельная проверка свежего nightly в CI зелёная. CI на каждый PR гоняет тесты на хосте вместе со сверкой документации с кодом, все тесты ядра в QEMU в режимах UEFI и BIOS, загрузку настоящего ядра и сборку сайта документации. После мёржа в `master` сайт публикуется на GitHub Pages.

## Тулчейн {#toolchain}

Ядру нужен nightly Rust: оно использует нестабильные возможности (`custom_test_frameworks`, `abi_x86_interrupt`). Версия закреплена в `rust-toolchain.toml`, вместе с компонентами `rust-src`, `llvm-tools-preview` и целью `x86_64-unknown-none`. rustup ставит всё это сам при первой команде cargo в репозитории.

Почему версия закреплена, а не плавающая, — в решении [0004](../decisions/0004-pinned-nightly.md).

## Как обновить nightly {#bump}

1. Запустите проверку свежего nightly вручную: GitHub → **Actions** → **test** → **Run workflow** на ветке `master`. Задачи `latest nightly (uefi)` и `latest nightly (bios)` соберут и протестируют проект на самом свежем nightly. Эти же задачи запускаются автоматически каждый понедельник.
2. Если обе задачи зелёные, посмотрите в их логе версию компилятора (шаг «Install the latest nightly» печатает `rustc -V`).
3. В `rust-toolchain.toml` замените `channel = "nightly-ГГГГ-ММ-ДД"` на дату этого nightly. Дата канала на день позже даты в `rustc -V`: компилятор от 10 октября выходит в канале `nightly-<следующий день>`.
4. Откройте PR с одной этой правкой. Обычный CI проверит всё на новой версии.

Если свежий nightly красный, ничего делать не нужно: закреплённая версия продолжает работать. Разберитесь, что сломалось, до следующего обновления.

## Задачи CI {#jobs}

CI описан в трёх файлах: тесты — в `.github/workflows/test.yml`, документация — в `.github/workflows/docs.yml`, готовые образы — в `.github/workflows/release.yml`.

| Файл | Задача | Когда | Что делает |
|---|---|---|---|
| `test.yml` | `host unit tests` | каждый PR и push в `master` | `cargo test-core`, `cargo test-runner` и `cargo docs-check`: тесты без QEMU и сверка документации с кодом, быстро |
| `test.yml` | `cargo test (uefi)`, `cargo test (bios)` | после `host unit tests` | `cargo test --no-fail-fast` в QEMU и загрузка настоящего ядра: `OSEL_EXPECT="OSel ready to go!" cargo run`. Ставит Go (`actions/setup-go`), чтобы runner собрал `user-go/gohello` и сквозной тест Go прошёл |
| `test.yml` | `latest nightly (uefi)`, `latest nightly (bios)` | по понедельникам и при ручном запуске | на самом свежем nightly: `cargo test-core`, `cargo test-runner` и `cargo test --no-fail-fast`; без сверки документации и без загрузки настоящего ядра, не ждёт `host unit tests` |
| `docs.yml` | `build the site` | каждый PR и push в `master` | собирает обе локали на Diplodoc, проверяет HTML и перенаправления через `tools/docs-site.py`; линтер роняет сборку на ссылках на несуществующие страницы; битые якоря ловит `cargo docs-check` |
| `docs.yml` | `publish to GitHub Pages` | push в `master` и ручной запуск | публикует собранный сайт на GitHub Pages |
| `release.yml` | `build and verify release images` | каждый PR и push тега `v*` | собирает образы в профиле `release` и проверяет готовые `.img` в QEMU через UEFI и BIOS, с обязательным тетрисом |
| `release.yml` | `publish the verified images` | push тега после успешной проверки обоих образов | публикует GitHub Release с двумя образами и SHA-256; порядок и восстановление после ошибки — в [выпуске версии](release.md#images) |

Новый push в PR отменяет устаревший прогон этого PR. Прогоны на `master` не отменяются никогда.

### Кэш {#cache}

- Сборки Rust кэшируются действием `Swatinem/rust-cache` для трёх проектов: корневого, `runner/` и `tools/docs-check/`.
- Прошивка OVMF кэшируется отдельно в `runner/.ovmf`, ключ кэша зависит от `runner/Cargo.lock`.

## Проверка документации {#docs-check}

`cargo docs-check` сверяет документацию с кодом: каждый модуль описан на карте модулей, каждая переменная runner есть в справочнике, каждый обработчик прерываний есть в таблице векторов, каждая блокировка описана, все упомянутые пути к коду существуют, все страницы есть в оглавлении своей локали, пути и якоря переводов совпадают, внутренние ссылки существуют. Правила — на странице [Как писать документацию](writing-docs.md).

## Частые ошибки {#errors}

| Что видно | Что делать |
|---|---|
| Красный только `latest nightly` | см. [Как обновить nightly](#bump); обычный CI не затронут |
| Тест таймера падает только в CI | [Что пошло не так](troubleshooting.md#symptoms) |
| `failed to fetch OVMF firmware` в CI | сеть GitHub на время недоступна; перезапустите задачу |
| Задача `build the site` падает на ссылке | ссылка ведёт на страницу, которой нет или которой нет в `toc.yaml`; исправьте ссылку или добавьте страницу в оглавление |
| `host unit tests` падает на шаге `Docs match the code` | `cargo docs-check` нашёл расхождение документации с кодом; в сообщении сказано, что и на какой странице добавить |
