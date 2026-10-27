# Локальная конфигурация

`/etc/gitcraft/config/agent.yml` — обычный YAML. UI изменяет те же валидируемые настройки; агент не зависит от панели. Секреты находятся отдельно в root-owned `/etc/gitcraft/secrets.env` (0600), systemd передает их через окружение.

```yaml
repository:
  url: git@github.com:your-team/minecraft-config.git
  branch: main
  poll_interval_seconds: 60
  authoring_path: ""
paths:
  repository: /var/lib/minecraft/repository
  server: /var/lib/minecraft/server
  state: /var/lib/minecraft/state
  cache: /var/lib/minecraft/cache
  staging: /var/lib/minecraft/staging
  backups: /var/lib/minecraft/backups
deployment:
  service: minecraft.service
  empty_grace_seconds: 60
  health_timeout_seconds: 180
rcon:
  host: 127.0.0.1
  port: 25575
  password_env: MINECRAFT_RCON_PASSWORD
  timeout_seconds: 5
web:
  host: 127.0.0.1
  port: 8765
  token_env: GITCRAFT_WEB_TOKEN
transfer:
  max_session_bytes: 107374182400
  max_staging_bytes: 214748364800
  max_files: 100000
  min_free_bytes: 1073741824
  session_ttl_seconds: 604800
```

## Что означают параметры

- `repository.url`: SSH или HTTPS без credentials. Рабочему агенту достаточно чтения.
- `repository.branch`: единственная deploy-ветка. Ветки разработчиков не становятся релизом до merge.
- `repository.authoring_path`: по умолчанию пусто (редактор выключен). Для UI editor задайте абсолютный путь отдельного обычного рабочего clone, например `/var/lib/minecraft/authoring`, вне всех шести managed paths. Он не может быть symlink, runtime, bare repository или staging; origin должен совпадать с `repository.url`. Редактор доступен только для существующих approved config paths. Обеспечьте Unix-права и systemd ReadWritePaths отдельно.
- `poll_interval_seconds`: период опроса Git. Текущий systemd запускает один долгоживущий `agent` без `--once`, который перечитывает YAML; старый timer отключается установщиком. Не запускайте второй timer параллельно.
- Шесть `paths`: абсолютные отдельные каталоги, не вложенные друг в друга. Для adopt рабочий server должен точно совпадать с existing unit WorkingDirectory. Не меняйте пути state/cache без миграции соответствующих файлов.
- `empty_grace_seconds`: как долго проверять отсутствие игроков до остановки.
- `health_timeout_seconds`: сколько ждать systemd + RCON после старта.
- `password_env`/`token_env`: имена env, не сами секреты. Web token минимум 24 символа. Fresh installer генерирует случайные значения; adopt использует уже действующий RCON пароль.
- Web и RCON client ограничены loopback IP. Это не ограничивает Minecraft listener; firewall настраивается отдельно.
- `transfer`: квоты сессии, суммарного staging, количества файлов, резерв свободного диска и TTL. Размеры указаны в байтах. Staging quota не меньше session quota. Максимум файлов 1 000 000, TTL не больше 30 дней. Browser upload ограничивает файл 64 MiB; для больших файлов есть CLI с chunks 1 MiB.

Произвольные дополнительные ключи не принимаются. Сохранение конфига атомарное, файл имеет 0600. Настройки systemd sandboxes и sudoers должны соответствовать выбранным путям/имени сервиса; изменение YAML само по себе не выдает новые системные права.
