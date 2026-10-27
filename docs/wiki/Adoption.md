# Подключение существующего сервера

Adopt предназначен для уже работающего Minecraft с существующим systemd unit. Он не переустанавливает Java, не меняет launch command, пользователя, unit, миры или пароль RCON.

## Подготовка

- Не запускайте fresh installer поверх существующего unit.
- Установите проверенный бинарник через `scripts/install.sh --mode adopt --service EXISTING.service --server-path /existing/path --repository git@github.com:TEAM/CONFIG.git --binary PATH` (сначала `--dry-run`) либо вручную подготовьте локальные сервисы/config. Adopt installer обнаруживает существующие User/Group/WorkingDirectory, сохраняет unit и Java; действующий RCON пароль передается через `--rcon-password-env ENV` или скрытый prompt. Используйте `--rcon-port`, если действующий порт отличается. Укажите точные `paths.server`, `deployment.service`, отдельные state/cache/staging/backups.
- Предоставьте агенту только нужный доступ к конфигам/state/cache и ограниченные операции systemctl выбранного сервиса. Не выдавайте полный root sudo.
- Передайте существующий действующий RCON пароль через `rcon.password_env`; не сбрасывайте его ради adopt. RCON должен быть закрыт извне.
- Экспортируйте и одобрите конфиги **текущей сборки**. Engine filename в manifest должен совпадать с JAR в существующем launcher. Каждый runtime JAR обязан совпасть с pinned SHA.

## Read-only план

```sh
gitcraft adopt plan --config /etc/gitcraft/config/agent.yml \
  --server /srv/minecraft --output /tmp/adopt-plan.json
```

Проверьте unit, WorkingDirectory, User, ExecStart, найденный Java path, FragmentPath, fingerprint и список сохраненных свойств. Сложный launcher не должен интерпретироваться как автоматически установленная совместимость. Если путь или unit изменился, сформируйте новый план.

## Применение

Локальный config clone должен быть чистым и находиться на exact одобренном commit deploy-ветки. Подставьте настоящий 40-символьный SHA:

```sh
gitcraft adopt apply --config /etc/gitcraft/config/agent.yml \
  --plan /tmp/adopt-plan.json --repository /home/operator/minecraft-config \
  --expected-commit APPROVED_40_CHARACTER_COMMIT_SHA
```

Apply проверяет, что SHA совпадает с локальным чистым HEAD **и актуальным remote branch**, повторно проверяет unit fingerprint, конфиги, JAR и RCON. Затем создает initial managed ownership state и проверенные cache receipts. Работающие игроки допустимы: runtime файлы при adopt не заменяются.

При несовпадении **не исправляйте мир/пароль автоматически**. Выясните, какая версия конфигов и JAR действительно работает, согласуйте Git состояние и повторите план. До успешного adopt долгоживущий agent service лучше не включать. После adopt дальнейшие обновления проходят обычный цикл Git→empty server→deploy.
