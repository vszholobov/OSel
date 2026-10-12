---
description: Разводящая страница документации OSel — что это за система и где найти нужное.
---

# OSel

OSel — учебная операционная система на Rust для компьютеров x86_64. Загружается через UEFI и через старый BIOS, работает в QEMU и на реальных ноутбуках с флешки. Цель — запустить на ней консольный тетрис, написанный на Go.

Сейчас готов этап 1: прерывания, память, куча, экран падения и тесты. Что дальше — в [планах](roadmap.md).

## Я хочу… {#i-want}

| Задача | Страница |
|---|---|
| Собрать и запустить OSel | [Быстрый старт](start/quickstart.md) |
| Загрузить OSel на ноутбуке с флешки | [Запуск с флешки](start/usb.md) |
| Понять красный экран и сообщить о сбое | [Красный экран](start/crash-report.md) |
| Узнать, поддерживается ли мой компьютер | [Samsung NP530U4B](reference/hardware/samsung-np530u4b.md), [Acer Nitro](reference/hardware/acer-nitro.md) |
| Добавить тест | [Тесты](guides/testing.md) |
| Разобраться, почему упало | [Отладка](guides/debugging.md), [Типичные проблемы](guides/troubleshooting.md) |
| Понять, почему упал CI | [Типичные проблемы](guides/troubleshooting.md) |
| Обновить nightly или разобраться в CI | [Тулчейн и CI](guides/toolchain-and-ci.md) |
| Выпустить версию | [Выпуск версии](guides/release.md) |
| Понять, как устроена система | [Обзор](architecture/overview.md) |
| Найти, где в коде что-то лежит | [Карта модулей](architecture/overview.md) |
| Узнать, почему сделано именно так | [Решения](decisions/index.md) |
| Узнать, что делать дальше | [Планы](roadmap.md), [Техдолг](debt.md) |
| Дополнить документацию | [Как писать документацию](guides/writing-docs.md) |

## Пользователю {#user}

Достаточно трёх страниц: [быстрый старт](start/quickstart.md), [запуск с флешки](start/usb.md) и [что делать с красным экраном](start/crash-report.md). Всё остальное — для разработки.

## Разработчику {#developer}

1. [Обзор](architecture/overview.md): из чего состоит система и как она загружается.
2. [Как устроены тесты](architecture/testing.md) и [рецепты тестов](guides/testing.md): любое изменение идёт вместе с тестами.
3. Подсистемы: [память](architecture/core/memory.md), [прерывания](architecture/core/interrupts.md), [блокировки](architecture/core/locking.md), [экран](architecture/drivers/console.md), [последовательный порт](architecture/drivers/serial.md), [клавиатура](architecture/drivers/keyboard.md).
4. Справочник: [runner](reference/runner.md), [векторы прерываний](reference/vectors.md).
5. [Решения](decisions/index.md) и [планы](roadmap.md).

## ИИ-агенту {#agent}

Начинай с файла `AGENTS.md` в корне репозитория: там команды, определение готовности, правила проекта и маршруты «задача → страницы». Claude Code подгружает его сам через `CLAUDE.md`, а правила из `.claude/rules/` — когда открываешь файлы соответствующей папки.

На опубликованном сайте рядом с этой страницей лежат `llms.txt` (оглавление с описаниями страниц) и `llms-full.txt` (полный текст документации одним файлом).

## Исходники и история {#source}

- Репозиторий: [github.com/vszholobov/OSel](https://github.com/vszholobov/OSel)
- История изменений: `CHANGELOG.md` в корне репозитория. Документация всегда описывает текущее состояние ветки `master`.
