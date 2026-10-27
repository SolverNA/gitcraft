# Экспорт конфигов и политика Git

## План до копирования

`export plan` читает локальную установленную сборку, показывает кандидатов, исключения, выбранные файлы и манифесты. Он не переносит миры или JAR и не пишет в сервер. Каждый config выбирается точным `--include`: расширение `.yml` само по себе не означает безопасность. Например, Essentials/userdata YAML — игровые данные, а не настройки.

`export apply --reviewed` означает, что оператор прочитал план и исходные конфиги. До записи проверяются исходные SHA. Изменились файлы — создайте новый план. Destination должен отсутствовать. Создается новый локальный Git repo без автоматического commit/push и без перезаписи чужого Git.

## Metadata для JAR

Пример **локального файла metadata**, не готовый релиз:

```json
{
  "artifacts": [
    {
      "path": "server.jar",
      "category": "engine",
      "filename": "server.jar",
      "name": "My approved engine",
      "version": "unversioned-local",
      "source": {"provider": "transfer"}
    },
    {
      "path": "plugins/Example.jar",
      "name": "Example",
      "version": "1.2.3",
      "source": {"provider": "transfer"}
    }
  ]
}
```

Замените name/version фактическими данными. `unversioned-local` — только явная метка оператора, когда версия неизвестна; программа не угадывает версию из имени файла. `path` — имя исходного файла, `filename` — имя рабочего JAR на целевой машине (по умолчанию basename исходника). Для fresh установки используйте `server.jar` либо настройте `install.sh --engine` с тем же именем; при adopt укажите существующий basename launch command. SHA-256 считается по локальным байтам. Для неизвестного источника используйте `transfer`: URL не придумывается.

Для известного источника `source.provider` может быть `direct` или `modrinth`, а поле metadata `url` содержит настоящий credential-free HTTPS URL этого точного файла. Для автообновления Modrinth также нужны `project`, `minecraft`, `loader`, `channel: release`. Проверьте источник и совместимость сами. Не кладите токены в URL.

Export формирует `requirements/engine.json` (объект), `plugins.json` (`plugins` array), `mods.json` (`mods` array). Их `pinned` содержит `version`, `sha256` и либо `url`, либо `transfer: true`. Transfer pin устанавливается только после отдельного импорта байтов и receipt в кеш.

## Общая policy

Один Rust policy используется при export, validate/CI, PR и staging до остановки сервера. `.gitignore` — дополнительная защита, а не проверка безопасности. Аудит читает **tracked** файлы, даже принудительно добавленные через `git add -f`, и неигнорируемые untracked файлы.

Запрещены миры и markers (`level.dat`, region), arbitrary world из `level-name`, playerdata/stats/advancements, database/runtime/cache/logs, JAR/архивы, `.env`, private keys и известные секретные значения. YAML/JSON/TOML разбираются, вложенные ключи и элементы списков проверяются. `export-policy.json` хранит точный список явно одобренных arbitrary config paths. Добавляя новый config, добавляйте его точный repo-relative путь в approvals и обязательно просматривайте PR.

Единственная автоматическая redaction при экспорте — удаление `rcon.password` из server.properties с отображением ключа в плане. Другие потенциальные секреты блокируют экспорт и требуют ручной санитаризации. Эвристика может дать ложный запрет или пропустить неизвестный формат: она **не гарантирует отсутствие всех секретов**. Ревью исходных выбранных файлов обязательно.

Если секрет уже закоммичен, удаление файла новым коммитом не удаляет историю. Отзовите секрет и отдельно согласуйте очистку истории; GitCraft не делает force-push самостоятельно.
