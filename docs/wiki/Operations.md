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


## Включение автоматических обновлений после инициализации

Успешные `bootstrap` и `adopt apply` сохраняют владение установленной сборкой, освобождают deploy lock и затем пытаются включить и запустить **только установленный стандартный** `gitcraft-agent.service`. Это общий путь CLI и панели. Сервис проверяет Git постоянно с `repository.poll_interval_seconds`; Web UI сам не заменяет poller.

В `/operations` и статусе панели показаны отдельно фактические `active` и `enabled`. `unknown` означает невозможность подтвердить состояние, а не выключенный сервис. Если Minecraft инициализирован, но активация отказала, результат — `partial_success`, состояние операции — `activation_required`, с сохранённым `initialized: true`. Не повторяйте bootstrap/adopt: исправьте указанную причину и нажмите подтверждаемое включение агента в `/operations` либо выполните:

```sh
gitcraft agent-activate --config /etc/gitcraft/config/agent.yml
```

Активация сверяет installed config, владельца и точный стандартный root-owned unit/бинарник, отсутствие recovery и сохранённое состояние инициализации. Она не переписывает произвольные units. Для старой установки может потребоваться системное обновление узкого sudoers-разрешения установщиком: браузер не получает произвольный root shell. Локальный/custom config, не соответствующий установленному стандартному сервису, показывает причину отказа; запуск Minecraft не выдаётся за включённую автоматизацию. Успех подтверждается только наблюдаемыми `active` и постоянным `enabled`.


## Minecraft при загрузке машины и обслуживание

Автозапуск Minecraft — отдельный выбор от включения `gitcraft-agent.service`. Успешная инициализация с `boot_policy_required: true` отображается как `setup_required`, а не как ошибка или полная готовность. Повторять bootstrap/adopt не нужно. После bootstrap/adopt в `/operations` нужно явно выбрать `enabled` или `disabled` и подтвердить политику. Adopt сохраняет прежнее состояние unit до этого выбора. Панель показывает желаемую политику и фактический systemd UnitFileState (`enabled`, `disabled`, `masked` или `unknown`); неподтверждённое состояние не выдается за выключенный unit. Настройка boot-политики сама не запускает Minecraft немедленно.

```sh
gitcraft minecraft-autostart --config /etc/gitcraft/config/agent.yml \
  --policy enabled --confirmed
# либо --policy disabled --confirmed
```

Остановка через GitCraft для обслуживания сохраняет durable maintenance state и временно отключает boot-автозапуск, сохраняя желаемую политику. Агент не запускает намеренно остановленный сервер даже после изменения Git или перезагрузки хоста. Явный `resume` проверяет установленную сборку, запускает её и восстанавливает выбранную boot-политику только после успешной готовности. Ошибка старта оставляет сервер остановленным и требует разбирательства; отказ отключения unit не скрывается за успешным maintenance-статусом. Если Minecraft уже готов, но восстановление boot-политики отказало, сообщение прямо указывает это частичное состояние: hold сохраняется. После исправления разрешений используйте подтверждённый maintenance stop на пустом сервере и явный resume; не повторяйте bootstrap/adopt.

Ручной `systemctl stop` за пределами GitCraft не создаёт maintenance marker. При enabled-политике последующая перезагрузка машины запустит такой unit; для устойчивого обслуживания используйте подтверждаемую операцию GitCraft. Старые установки требуют обновлённого root-owned файла разрешённого unit и узких sudoers-разрешений для конкретного Minecraft unit, если их ещё нет. Такой отказ — обязательный шаг системного обновления прав, а не включённый автозапуск. Ни панель, ни агент не переписывают существующий adopted unit и не получают произвольный root shell.


Shell-helper первого запуска также принимает явный выбор, например `sudo scripts/bootstrap-server.sh --accept-eula --autostart enabled` (либо `disabled`). Значения по умолчанию, молча включающего boot, нет. Без `--autostart` прежняя политика сохраняется и выводится обязательный следующий шаг явного выбора; этот запуск не считается завершённой настройкой автозапуска. При работе через браузер после системной установки этот выбор выполняется отдельной подтверждаемой формой; редактировать YAML ради него не нужно.
