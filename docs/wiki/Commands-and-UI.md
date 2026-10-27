# Команды и Web UI

Полный синтаксис всегда доступен через `gitcraft --help` и `gitcraft COMMAND --help`.

| Команда | Назначение |
|---|---|
| `validate --repository PATH` | Общий policy, tracked audit, синтаксис и manifests |
| `export plan/apply` | Локальная выборка конфигов с preview |
| `propose --repository PATH --base BRANCH --title TEXT` | Commit в новую ветку и PR |
| `agent --config FILE [--once]` | Постоянный poll или один reconciliation |
| `deploy --config FILE` | Одна обычная попытка с проверкой игроков |
| `status --config FILE` | Состояние установки и ожиданий |
| `bootstrap --config FILE --accept-eula` | Явный первый старт остановленного сервера |
| `adopt plan/apply` | Подключить existing unit без перезаписи |
| `transfer plan/send/status/verify/commit/recover` | Независимый перенос байтов |
| `recover --config FILE [--acknowledge-data-risk]` | Восстановить прерванную managed transaction |
| `backup-data --config FILE --include REL` | Отдельная offline копия игровых данных |
| `update-dependencies --repository PATH` | Локально подготовить новые pins без merge |
| `web --config FILE` | Loopback панель |

## Панель

UI — оболочка над теми же Rust operations и текстовыми конфигами. Открывайте через SSH tunnel. Вход admin token; cookie HttpOnly/SameSite и CSRF защищают browser POST. API transfer имеет отдельные ограниченные session tokens.

Панель показывает установленное состояние, ожидающий commit/причину ожидания и ошибки, дает безопасную попытку deploy, настройки repository/branch и onboarding, plan/apply export/adopt, перенос файлов. Все долгие операции должны выполняться в фоне; deploy защищен межпроцессной блокировкой. Отображаемые тексты/пути экранируются; секретные config values не должны появляться в preview.

Локальные параметры можно менять в YAML без UI. Репозиторные конфиги проходят Git/PR, а не прямое редактирование рабочих Minecraft файлов на VPS. При заданном `repository.authoring_path` редактор `/editor` открывает только существующие approved config paths отдельного clone (до 64 KiB), показывает preview/pending diff и предлагает PR. Для CLI review используйте `git diff` и `propose`; workflow/системные изменения согласуются отдельно.

## Состояние

Основные файлы `paths.state`: `deployment.json` (последний успешный commit/ownership), `status.json`, `transaction.json` (незавершенный deploy), `last-transaction.json`, scoped transfer sessions. Не удаляйте state ради обхода ошибки. Потеря ownership может привести к тому, что удаленный из Git старый plugin останется на диске.
