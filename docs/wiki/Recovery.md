# Резервные копии и восстановление

## Два разных backup

1. **Managed backup**: прежние configs/JAR, их hashes и transaction journal. Нужен для восстановления файлов деплоя.
2. **Game-data backup**: миры, userdata, локальные БД плагинов. Это отдельная операция и отдельная ответственность. Внешний SQL-сервер не копируется командой локальных файлов.

Перед обновлением с возможной миграцией остановите Minecraft и сделайте согласованную копию:

```sh
gitcraft backup-data --config /etc/gitcraft/config/agent.yml \
  --include OurRealm --include OurRealm_nether \
  --include plugins/Example/database
```

Команда требует остановленного сервиса, явных относительных путей и проверяет состояние после копирования. Она не останавливает/не запускает Minecraft сама и не может защитить от другого администратора, параллельно запускающего сервис. Внешнюю БД снимайте отдельным согласованным способом.

## Ошибка до остановки

Неверный hash, unsafe config, ошибка Git или непонятный RCON прерывают подготовку до изменения runtime. Устраните причину, отправьте новый approved commit при необходимости. Никогда не считайте неработающий RCON нулевым онлайном.

## Ошибка после запуска новой Java

Новый плагин/ядро уже мог изменить мир или БД. Поэтому автоматический downgrade с повторным стартом старой версии не выполняется. Сервер остается остановленным, сохраняются backup/journal, status содержит `recovery_required`.

Проверьте журнал, версии, согласованный data backup и допустимость возврата. Только после осознанной проверки:

```sh
gitcraft recover --config /etc/gitcraft/config/agent.yml --acknowledge-data-risk
```

Команда проверяет hashes managed backup, восстанавливает только управляемые файлы и прежний deployment state. **Мир/БД не восстанавливаются, сервис не запускается.** Отдельно верните соответствующие игровые данные при необходимости, проверьте их и используйте Resume в `/operations` или `gitcraft resume --acknowledge-data-risk`. Проверяются launcher, exact installed commit, hashes и stopped state; перед стартом накладывается current local RCON credential.

## Crash или потеря питания

`transaction.json` записывается durable до опасных стадий. Следующая попытка deployment не должна молча продолжать поверх незавершенной транзакции. Если новая Java могла стартовать, требуется тот же явный risk acknowledgement. При поврежденном backup hash восстановление прерывается до записи target.

```sh
gitcraft status --config /etc/gitcraft/config/agent.yml
gitcraft recover --config /etc/gitcraft/config/agent.yml
```

Вторая команда без acknowledgement допустима только если journal подтверждает, что Java еще не стартовала. По результату сервис остается остановленным. Не удаляйте journal вручную, чтобы заставить deploy идти дальше; сначала расследуйте состояние.

World transfer использует собственный journal и `transfer recover`; он возвращает сохраненные каталоги, а не сливает игровые изменения.

## Хранение

Автоматической retention policy для backups/cache/staging пока нет. Следите за диском, храните нужные восстановления и удаляйте старые копии по собственной политике. Сохраните копию backup вне единственного диска VPS и проверьте восстановление на стенде.

Операционные settings, offline backup/list, stop, EULA bootstrap и recovery доступны в [панели](Operations.md). Нельзя обойти pending journal переносом state path: guard это блокирует. Отмена/cleanup сессий отделена от world recovery и не удаляет pending world journal независимо от TTL.
