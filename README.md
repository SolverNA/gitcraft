# GitCraft

**Minecraft GitOps на Rust:** настройте сборку локально, выберите конфиги для Git, отправьте изменение через PR и разверните одобренный commit на сервере, когда игроков нет. Миры, JAR, базы плагинов и секреты остаются вне Git.

📖 **[GitHub Wiki](https://github.com/SolverNA/gitcraft/wiki)** · **[Версионируемая документация](docs/wiki/Home.md)** · [Быстрый старт](docs/wiki/Quickstart.md) · [Исходная идея](IDEA.md)

## Как проходит работа

```text
Локальный Minecraft → export plan → review → config Git → PR → merge deploy-ветки
                                                               ↓
                                    VPS: staging + hashes → пустой онлайн → deploy

Миры и JAR → отдельный authenticated transport → verify → явный commit на VPS
```

Один репозиторий хранит этот инструмент, другой — настройки конкретной сборки. Агент следит только за `repository.branch` и устанавливает точный SHA; ветка разработчика не становится релизом до merge. Бот зависимостей предлагает стабильные совместимые версии через PR.

## Быстрый вход

```sh
cargo build --locked --release
cargo test --locked
./target/release/gitcraft --help
```

Нужны Rust/Cargo 1.95.0 и Git для сборки; Python на сервере не требуется. Рабочая установка поддерживает Debian/Ubuntu с systemd и подходящей для вашей сборки Java.

1. **[Экспортировать локальную сборку](docs/wiki/Export-and-policy.md):** `export plan` с точным выбором файлов и metadata JAR, затем `export apply --reviewed` в новый каталог. План не выводит исходные значения конфигов, а применение повторно проверяет SHA. Неизвестный источник JAR задается явно как `transfer`, без выдуманных URL.
2. **[Настроить Git и PR](docs/wiki/Pull-requests.md):** отдельный config repo, protected deploy-ветка, CI. `propose` создает feature branch и PR; прямой push в deploy-ветку не выполняется.
3. **[Пустой VPS](docs/wiki/Transfer.md):** installer dry-run → установка → закрыть внешний RCON → прямой перенос мира/JAR → verify/commit → явный EULA bootstrap.
4. **[Существующий VPS](docs/wiki/Adoption.md):** `install.sh --mode adopt`, read-only plan и exact-SHA apply; существующие unit, Java, User, миры и RCON credentials сохраняются.

## Что входит

- Общая policy для export, Git/CI, PR и deploy staging: tracked-file audit, явные approvals произвольных конфигов, world/runtime/binary exclusions и эвристика секретов.
- Mappings `server-settings/`, `plugins-configs/`, `config/`, `defaultconfigs/`.
- HTTPS pins с SHA-256 и отдельные transfer-backed pins с проверенными receipt.
- Прямой resumable transport: authentication, scoped sessions, chunks, offsets, SHA, квоты, verify и явное подтверждение замены мира.
- Loopback Web UI: полные guarded settings, write-only credentials, bootstrap/EULA, stop/resume/recovery, offline backup/list, approved-config editor/diff/PR/retry и browser transfer/lifecycle cleanup. CLI и YAML остаются доступны.
- Настраиваемый poll в долгоживущем systemd agent, автоактивация poller после bootstrap/adopt, отдельный явный выбор Minecraft boot-политики, actual active/enabled и отдельный retry при частичном успехе; RCON fail-closed и межпроцессная блокировка.
- Durable journal, backup hashes, осознанное stopped recovery после старта новой Java; отдельные offline game-data backups.

## Руководства

| Задача | Страница |
|---|---|
| Понять устройство | [Архитектура](docs/wiki/Architecture.md) |
| Полный цикл без ручного YAML после установки | [Операции панели](docs/wiki/Operations.md) |
| Сессии: status/cancel/reviewed cleanup | [Lifecycle](docs/wiki/Session-lifecycle.md) |
| Все поля YAML и квоты | [Конфигурация](docs/wiki/Configuration.md) |
| Команды и UI | [Справочник](docs/wiki/Commands-and-UI.md) |
| Миры/JAR и обрыв загрузки | [Перенос](docs/wiki/Transfer.md) |
| Unit уже существует | [Adopt](docs/wiki/Adoption.md) |
| Ветки, источники и review | [PR](docs/wiki/Pull-requests.md) |
| Секреты, сеть и права | [Безопасность](docs/wiki/Security.md) |
| Ошибка старта/crash/backup | [Восстановление](docs/wiki/Recovery.md) |
| Что делать при ошибке | [Диагностика](docs/wiki/Troubleshooting.md) |

Страницы опубликованы в [GitHub Wiki](https://github.com/SolverNA/gitcraft/wiki) и версионируются вместе с кодом в `docs/wiki/`. Копия в исходниках позволяет читать руководство для конкретного commit.

## Проверка разработки

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
bash scripts/test-install.sh
```

Локальный E2E использует реальные временные Git repositories, ветку/merge, экспорт, перенос файлов и hashes/receipts. Service/RCON/stopped checks подменены; это не тест реального JVM, production VPS или правил GitHub branch protection. Отдельный PR-тест использует mocked `gh` и настоящий bare remote.

## Существенные границы

- До первого старта закройте внешний TCP RCON для IPv4 и IPv6. Loopback client не ограничивает Minecraft listener. Firewall/SSH установщик не меняет; Web UI открывайте через SSH tunnel.
- Secret scanning эвристический, а не гарантия. Каждый выбранный config требует ручного review. Плагины считаются доверенным кодом: fresh MVP использует один UID для Java, агента и UI.
- После попытки запуска новой Java автоматический downgrade запрещен: данные уже могли мигрировать. [Recovery](docs/wiki/Recovery.md) восстанавливает managed files и оставляет сервис остановленным; мир/БД требуют соответствующей отдельной копии.
- Browser upload ограничен 64 MiB на файл; более крупные файлы отправляются streaming CLI. Источник должен быть остановлен или быть заранее согласованным snapshot.
- Retention backup/cache/staging не автоматизирована. Поддерживается управляемый JAR launcher; установка сложных Forge libraries/argfiles не угадывается. Modrinth — поддерживаемый auto-update provider; неизвестные/прочие источники закрепляются вручную.

Браузерная приемка запускается настоящим Playwright/Chromium: `npm ci --ignore-scripts`, `npx playwright install chromium`, `npm run test:browser`. [Покрытие и внешние границы](docs/testing/browser-acceptance.md). Launcher проверяется по точным systemd/busctl argv; env-based и ambiguous units требуют отдельной reviewed migration при системной установке, автоматически не переписываются.
