# Быстрый старт

## 1. Собрать инструмент

На машине разработчика установите Rust/Cargo 1.95.0 и Git:

```sh
cargo build --locked --release
cargo test --locked
./target/release/gitcraft --help
```

Далее `gitcraft` означает этот бинарник в PATH. Minecraft на рабочей машине должен уже иметь проверенную сборку. Нужны абсолютные пути, отдельный каталог нового config-репозитория и достаточно свободного места для последующего переноса.

## 2. Сделать план конфигов

Сначала получите список кандидатов без выбора файлов:

```sh
gitcraft export plan --server /srv/local-minecraft --output /tmp/export-discovery.json
```

Посмотрите JSON. Затем создайте отдельный локальный `metadata.json` по [формату](Export-and-policy.md): для ядра и каждого плагина/мода укажите реальные name/version/source. Для неизвестного происхождения явно выберите `provider: transfer`; это не автоматическое доверие к файлу и не автообновляемый источник.

```sh
gitcraft export plan --server /srv/local-minecraft \
  --include server.properties \
  --include spigot.yml \
  --include plugins/Example/config.yml \
  --include config/example.toml \
  --metadata /tmp/metadata.json --output /tmp/export-plan.json
```

Проверьте `selected`, `blocked`, `unresolved_artifacts`, `manifests`, `redacted_keys`. Прочитайте **сами выбранные конфиги**: эвристика секретов не полная. Исправьте секреты вручную и пересоздайте план. План не содержит исходных значений конфигов.

```sh
gitcraft export apply --plan /tmp/export-plan.json \
  --destination /home/operator/minecraft-config --reviewed
gitcraft validate --repository /home/operator/minecraft-config
```

Destination не должен существовать. JAR и миры не копируются в Git.

## 3. Настроить GitHub

Создайте отдельный приватный репозиторий настроек, добавьте origin в экспортированный рабочий репозиторий, проверьте diff и сделайте первоначальный commit. Первая публикация пустого репозитория — отдельная операция администратора; это не обычный релиз через PR.

Шаблоны Actions добавляются через `scripts/bootstrap-repo.sh`; для существующего config-репозитория скрипт предлагает PR. Нужны локальная авторизация `gh`, ссылка на репозиторий инструмента и **точный опубликованный 40-символьный SHA** инструмента. Текущий шаблон checkout другого репозитория предполагает его публичность; для private tool repo требуется отдельная авторизация. Не делайте проект публичным только ради этой команды.

В GitHub защитите выбранную deploy-ветку: обязательный PR, одобрения и проверки. В локальном `agent.yml` укажите **ту же** `repository.branch`.

## 4. Выбрать способ запуска

- [Пустой VPS](Transfer.md): установка сервиса → закрыть внешний RCON → передать игровые данные и JAR → verify/commit → явный EULA bootstrap.
- [Уже работающий VPS](Adoption.md): read-only adopt plan → проверка unit/config/JAR/точного SHA → adopt apply. Не запускайте fresh installer поверх существующего unit.

## 5. Повседневное изменение

В отдельном рабочем clone от актуальной deploy-ветки измените одобренный конфиг и выполните:

```sh
gitcraft validate --repository /home/operator/minecraft-config
gitcraft propose --repository /home/operator/minecraft-config \
  --base main --title 'Change server MOTD'
```

Если ваша deploy-ветка называется `release`, используйте `--base release` и `repository.branch: release`. Команда отправляет только новую ветку предложения. До merge агент не должен видеть изменение как релиз. После merge он скачает конкретный SHA, подготовит файлы и дождется пустого сервера.
