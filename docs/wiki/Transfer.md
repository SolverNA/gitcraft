# Новый VPS и прямой перенос данных

Миры и JAR передаются **клиент → VPS**, не через GitHub. Экспортированный config-репозиторий содержит только pins и настройки. Крупные файлы не нужно заливать в releases или artifacts CI.

## Подготовить пустой VPS

1. Проверьте Debian/Ubuntu с systemd, Java, свободный диск и резервный доступ по SSH.
2. Соберите инструмент на совместимой машине или на VPS. Выполните dry-run установщика, затем установите проверенный бинарник:

```sh
bash scripts/install.sh --repository git@github.com:your-team/minecraft-config.git --dry-run
sudo bash scripts/install.sh --repository git@github.com:your-team/minecraft-config.git --binary ./target/release/gitcraft
```

3. Добавьте выданный deploy public key в GitHub **без write**. SSH host key GitHub проверьте независимо. Не отключайте проверку host key.
4. До первого Minecraft start закройте внешний TCP RCON для IPv4 и IPv6. Установщик не меняет firewall и SSH.
5. Подключитесь к UI через SSH tunnel:

```sh
ssh -L 8765:127.0.0.1:8765 operator@your-vps
```

Откройте `http://127.0.0.1:8765`. Admin token берется локально из secrets.env без публикации в Git/логи. Для CLI задавайте его в окружении защищенным способом, не через argv.

## Консистентность источника

Перед переносом остановите исходный Minecraft и убедитесь, что он завершился, либо используйте согласованный read-only filesystem snapshot. `--source-consistency stopped|snapshot` — явное утверждение оператора о состоянии **источника**, не команда удаленной остановки. Работающий мир нельзя считать консистентным только потому, что отдельные SHA совпали. Не используйте живую меняющуюся папку.

## План и отправка CLI

Пример мира с нестандартным именем:

```sh
gitcraft transfer plan --source /srv/snapshot/OurRealm --kind world \
  --source-consistency stopped --output /tmp/world-transfer.json
```

Для JAR используется отдельный план и kind `engine`, `plugin` или `mod`:

```sh
gitcraft transfer plan --source /srv/snapshot/server.jar --kind engine \
  --source-consistency stopped --output /tmp/engine-transfer.json
```

Прочитайте план с именами, размерами и SHA. Отправьте его через tunnel:

```sh
gitcraft transfer send --endpoint http://127.0.0.1:8765 \
  --plan /tmp/world-transfer.json --session /tmp/world-session.json
```

CLI получает ограниченную сессию, отправляет chunks, сохраняет session file 0600. Этот файл содержит scoped token и не должен попадать в Git. При обрыве повторите ту же команду с тем же `--session`: подтвержденные offsets/chunks проверяются и загрузка продолжается. `send` не делает мир рабочим автоматически.

```sh
gitcraft transfer status --endpoint http://127.0.0.1:8765 --session /tmp/world-session.json
gitcraft transfer verify --endpoint http://127.0.0.1:8765 --session /tmp/world-session.json
```

## Применить отдельно

Целевой Minecraft должен быть остановлен. Каталоги `paths.server` и `paths.backups` должны находиться на одной файловой системе: текущий world commit сохраняет старый каталог через atomic rename (fresh installer размещает их в `/var/lib/minecraft`). При EXDEV сервер остается остановленным, сохраненный journal требует explicit recover; cross-filesystem copy-backup автоматически не выполняется. Для миров всегда требуется явное подтверждение замены, включая миграцию на пустой VPS:

```sh
gitcraft transfer commit --endpoint http://127.0.0.1:8765 \
  --session /tmp/world-session.json --confirm-world-replace
```

Существующий мир сохраняется вне server в backup. После commit проверьте receipt и наличие нужных файлов; отдельно перенесите Nether/End, если это разные каталоги. Укажите такое же `level-name` в approved server.properties. JAR commit импортирует проверенный кеш и receipt; Git pin должен иметь ровно тот же SHA.

Повторите send→verify→commit для engine/plugins/mods. Не меняйте версии во время миграции: сначала перенесите точную проверенную сборку, затем обновляйте через отдельные PR.

## Первый старт

Проверьте approved branch, pins, launcher filename, закрытый внешний RCON и принятые вами условия Minecraft EULA. Затем из проверенного checkout инструмента:

```sh
sudo bash scripts/bootstrap-server.sh --accept-eula --autostart enabled
```

Helper читает локальные secrets как данные, запускает bootstrap от пользователя minecraft и включает Minecraft и долгоживущий agent service только после успешной готовности. При нестандартном unit/User/launcher запустите CLI с проверенным config и окружением вместо helper, привязанного к стандартной fresh установке. Bootstrap не должен генерировать новый мир вместо переданного: перед запуском проверьте точное имя мира/папку и перенос всех dimensions.

## Browser UI и ограничения

Browser-перенос использует прямой authenticated API, scoped tokens, progress/offsets, verify и явный commit. Каждый файл ограничен 64 MiB для безопасного browser hashing; большие `.mca` или JAR отправляйте CLI. Browser session содержит секрет: не публикуйте его. Не используйте символические ссылки, traversal paths или архивы — система переносит отдельные файлы и не распаковывает tar/zip.

## Прерванный world commit

Если commit прерван, новая попытка deploy блокируется до восстановления:

```sh
gitcraft transfer recover --endpoint http://127.0.0.1:8765 --session /tmp/world-session.json
```

Minecraft оставьте остановленным. Восстановление возвращает сохраненные старые world directories; оно не сливает две истории мира. Смотрите также [Recovery](Recovery.md).

Имя engine в metadata/export, transfer plan и fresh launcher должно совпадать. По умолчанию это `server.jar`; для другого basename используйте `install.sh --engine NAME.jar`, а в metadata `filename` явно укажите то же имя. Existing adopt unit не переименовывается автоматически.

## Управление сессиями и привязка назначения

[Lifecycle](Session-lifecycle.md) позволяет перечислить metadata, отменить idle сессию по ID и освободить staging после preview/review. Новая сессия закрепляет server/cache/backups/state-transfer root и service identity. Активный/verified/recovery перенос блокирует смену этих настроек, backend повторно сверяет binding при commit/recover. Token не разрешает перенести выбранные байты в другой сервер через изменение config. Старую сессию без binding нельзя молча commit: создайте новую. Legacy recovery возможен только при доказанном совпадении записанных stage/backup/current destination; иначе остановитесь и расследуйте.
