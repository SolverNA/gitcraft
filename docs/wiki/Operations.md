# Полный рабочий цикл в панели

После системной установки откройте `http://127.0.0.1:8765` через подготовленное защищенное подключение и войдите локальным admin token. **Операции → Полные настройки** доступны без ручного редактирования YAML. Панель не исполняет произвольные shell-команды и не создает systemd unit с пользовательским `ExecStart`.

## Все настройки через формы

В `/operations` доступны repository URL/branch/authoring clone, шесть managed paths, существующий service, RCON host/port/env reference, Web bind/port/token reference, интервалы и transfer quotas/TTL. Значения сначала проходят общий validator и показываются как план. Только после review сохраняется полный config атомарно с проверкой исходной revision.

При параллельном изменении файла нужен новый preview. План связан также с непрозрачной generation локального credential store: смена пароля после preview требует нового review. В план не включаются пароли или их хеши.

Пути не являются командой миграции. Изменение populated managed roots, ownership/service, незавершенного deploy/transfer или pending proposal блокируется, чтобы не потерять журналы и данные. Опасные изменения path/service/RCON требуют остановленного сервиса. Новый путь должен соответствовать Unix permissions и существующему systemd sandbox; новые root-права через браузер не выдаются. Для отдельных установок используйте отдельные config/state и reviewed adoption.

Web bind проверяется **до** записи: занятый адрес отклоняется, прежний config/listener сохраняются. При успешной смене Web адреса listener переключается без shell restart; переподключитесь по новому адресу и войдите снова. Если используете SSH port forwarding, его транспорт должен соответствовать выбранному порту — настройка внешней сети не является операцией панели.

## Credentials: только запись

Форма не читает и не показывает действующие значения. Можно записать RCON password или новый Web token. Локальный provider — отдельный owned regular file `local-secrets.json` рядом с `agent.yml`, режим 0600, без symlink/hardlink. Он не переносится в Git и не находится в authoring clone. Неподдерживаемые роли, malformed/public/чужой store отклоняются, а не обходятся молчаливым fallback.

Для роли, которой нет в private store, используется выбранная переменная окружения из установки. Пустой input не перезаписывает пароль. RCON credential меняется только при остановленном сервисе и явном подтверждении. При последующем resume новый credential накладывается в local server.properties до запуска. Web token rotation завершает прежние login sessions; войдите новым токеном, который вы только что задали. Сервер не возвращает его в ответ.

## Выбрать явную инициализацию

**Новый сервер:** перенести и проверить данные/JAR, проверить launcher и pins, затем форма «Первый запуск» с отдельным согласием Minecraft EULA. Без него backend также отклоняет запуск. Отсутствие initial ownership state не трактуется как обычное обновление даже на пустом работающем сервере.

**Уже существующий сервер:** reviewed adopt plan/apply, сохраняющий unit/Java/User/RCON. Если unit нельзя проверить как точный прямой Java launcher, операция завершается до остановки или записи, с объяснением. Панель не переписывает неизвестный launcher.

## Обслуживание и данные

- **Stop:** только известный нулевой online, save-all flush, повторная проверка. Игроков не кикают. Unknown RCON и вход игрока отклоняют остановку.
- **Backup:** после остановки выберите точные world/plugin-data paths, по одному на строку. Панель покажет созданные offline backups и количество файлов. Внешние SQL БД требуют отдельной согласованной процедуры.
- **Resume:** явное подтверждение совместимости данных; только stopped service, без pending journals, с проверкой installed exact commit, config/JAR hashes и launcher. Затем local RCON overlay, старт и готовность **systemd active + RCON**. Любая неоднозначная попытка запуска приводит к попытке stop и `recovery_required` с фактически проверенным `service_stopped`, а не обещанием успешной остановки.
- **Recovery:** прочитайте pending phase и риск запуска Java. После осознанного acknowledgement восстанавливаются managed files; мир/БД не откатываются и сервис остается остановленным.
- **PR retry:** повторить открытие PR для сохраненной проверенной ветки. Нового commit/push не происходит, dirty edits сохраняются отдельно.

Панель показывает `running`, `completed`, `rejected`, `error`, `recovery_required`. Результат `202` означает только запуск фоновой операции. После перезапуска Web process незавершенная UI-операция помечается как требующая проверки; она не повторяется автоматически.

## CLI эквиваленты

```sh
gitcraft config-show --config /etc/gitcraft/config/agent.yml
gitcraft config-apply --config /etc/gitcraft/config/agent.yml \
  --input /path/to/reviewed-agent-config.yml --expected-revision CURRENT_REVISION --reviewed
gitcraft credential-set --config /etc/gitcraft/config/agent.yml --role rcon --confirmed
gitcraft maintenance-stop --config /etc/gitcraft/config/agent.yml --confirmed
gitcraft backup-list --config /etc/gitcraft/config/agent.yml
gitcraft resume --config /etc/gitcraft/config/agent.yml --acknowledge-data-risk
```

`config-show` возвращает config object и revision, без значений secrets. `--input` принимает сам config object, не wrapper с revision. `credential-set` читает секрет из **stdin**, не argv; запускайте от service UID с доступом к private provider, не создавайте root-owned store, который сервис не сможет прочитать. CLI не заменяет проверку прав хоста или обязательность review.
