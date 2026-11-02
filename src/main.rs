//! Parenthesia 最小プロトタイプ。
//! Scheme (Steel) -> コマンドキュー -> Bevy システム(毎フレーム処理) の流れ。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use steel::rerrs::{ErrorKind, SteelErr};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

/// スクリプトのパスを上書きする環境変数。
const SCRIPT_ENV: &str = "PARENTHESIA_SCRIPT";

/// 現実の1秒 = ゲーム内1分。
const GAME_MINUTES_PER_REAL_SECOND: f32 = 1.0;
/// ゲーム開始時刻(分)。5:50 開始にして、日課の最初の切り替え(6時)がすぐ見えるようにしている。
const START_MINUTES: f32 = 5.0 * 60.0 + 50.0;
/// NPC の歩行速度 (px/秒)。
const NPC_SPEED: f32 = 60.0;

/// スクリプトのパス。環境変数 PARENTHESIA_SCRIPT があればそれを、
/// なければビルド時の CARGO_MANIFEST_DIR 基準の scripts/main.scm を使う。
/// (実行時のカレントディレクトリには依存しない)
fn script_path() -> PathBuf {
    std::env::var_os(SCRIPT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/main.scm"))
}

/// Scheme から積まれるコマンド。ECS には触れず、データだけを持つ。
#[derive(Debug)]
enum ScriptCommand {
    SpawnNpc {
        name: String,
        x: f32,
        y: f32,
    },
    /// 日課: (時, x, y) のリスト。
    SetSchedule {
        name: String,
        entries: Vec<ScheduleEntry>,
    },
}

#[derive(Debug, Clone, Copy)]
struct ScheduleEntry {
    hour: f32,
    pos: Vec2,
}

/// Scheme 側と Bevy 側で共有するキュー。
#[derive(Resource, Clone, Default)]
struct CommandQueue(Arc<Mutex<Vec<ScriptCommand>>>);

/// Steel の Engine。Send ではないので NonSend リソースとして保持する。
/// 後から Scheme の関数を呼び続けるため、破棄しない。
#[allow(dead_code)]
struct ScriptEngine(Engine);

/// ゲーム内時計(0.0..1440.0 分)。
#[derive(Resource)]
struct GameClock {
    minutes: f32,
}

#[derive(Component)]
struct Npc;

/// NPC の日課。時刻昇順に並べて保持する。
#[derive(Component)]
struct Schedule(Vec<ScheduleEntry>);

#[derive(Component)]
struct ClockText;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .init_resource::<CommandQueue>()
        .insert_resource(GameClock {
            minutes: START_MINUTES,
        })
        .add_systems(Startup, (setup_camera, setup_clock_ui, load_script))
        .add_systems(
            Update,
            (
                tick_clock,
                update_clock_text,
                process_commands,
                follow_schedule,
            )
                .chain(),
        )
        .run();
}

fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

fn setup_clock_ui(mut commands: Commands) {
    commands.spawn((
        ClockText,
        Text::new("00:00"),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            right: Val::Px(12.0),
            ..default()
        },
    ));
}

/// Steel の数値(整数・小数・有理数)を f32 に変換する。
/// FromSteelVal for f64 は NumV(小数)しか受け付けないため、SteelVal で受けて自前で変換する。
fn to_f32(v: &SteelVal) -> Result<f32, SteelErr> {
    match v {
        SteelVal::NumV(n) => Ok(*n as f32),
        SteelVal::IntV(n) => Ok(*n as f32),
        SteelVal::Rational(r) => Ok(*r.numer() as f32 / *r.denom() as f32),
        other => Err(SteelErr::new(
            ErrorKind::ConversionError,
            format!("数値が必要ですが {other} が渡されました"),
        )),
    }
}

/// '((時 x y) ...) を ScheduleEntry のリストに変換する。
fn parse_schedule(v: &SteelVal) -> Result<Vec<ScheduleEntry>, SteelErr> {
    let bad = |msg: String| SteelErr::new(ErrorKind::ConversionError, msg);
    let list = v
        .list()
        .ok_or_else(|| bad(format!("日課は ((時 x y) ...) のリストで指定してください: {v}")))?;
    list.iter()
        .map(|item| {
            let triple: Vec<&SteelVal> = item
                .list()
                .ok_or_else(|| bad(format!("日課の要素は (時 x y) で指定してください: {item}")))?
                .iter()
                .collect();
            let [h, x, y] = triple[..] else {
                return Err(bad(format!("日課の要素は (時 x y) の3要素です: {item}")));
            };
            Ok(ScheduleEntry {
                hour: to_f32(h)?,
                pos: Vec2::new(to_f32(x)?, to_f32(y)?),
            })
        })
        .collect()
}

/// 起動時に Engine を作って API を登録し、スクリプトを実行する。
/// Engine は NonSend リソースとして World に残す。
fn load_script(world: &mut World) {
    let queue = world.resource::<CommandQueue>().0.clone();
    let mut engine = Engine::new();

    // Scheme に公開する API。キューに積むだけで ECS には触れない。
    let q = queue.clone();
    engine.register_fn(
        "spawn-npc",
        move |name: String, x: SteelVal, y: SteelVal| -> Result<(), SteelErr> {
            let cmd = ScriptCommand::SpawnNpc {
                name,
                x: to_f32(&x)?,
                y: to_f32(&y)?,
            };
            q.lock().unwrap().push(cmd);
            Ok(())
        },
    );

    let q = queue;
    engine.register_fn(
        "set-schedule",
        move |name: String, schedule: SteelVal| -> Result<(), SteelErr> {
            let mut entries = parse_schedule(&schedule)?;
            entries.sort_by(|a, b| a.hour.total_cmp(&b.hour));
            q.lock()
                .unwrap()
                .push(ScriptCommand::SetSchedule { name, entries });
            Ok(())
        },
    );

    let path = script_path();
    match std::fs::read_to_string(&path) {
        Ok(src) => {
            if let Err(e) = engine.run(src) {
                error!("{} の実行に失敗: {e}", path.display());
            }
        }
        Err(e) => error!("{} を読み込めません: {e}", path.display()),
    }

    world.insert_non_send(ScriptEngine(engine));
}

fn tick_clock(time: Res<Time>, mut clock: ResMut<GameClock>) {
    clock.minutes =
        (clock.minutes + time.delta_secs() * GAME_MINUTES_PER_REAL_SECOND).rem_euclid(24.0 * 60.0);
}

fn update_clock_text(clock: Res<GameClock>, mut q: Query<&mut Text, With<ClockText>>) {
    let total = clock.minutes as u32;
    for mut text in &mut q {
        text.0 = format!("{:02}:{:02}", total / 60, total % 60);
    }
}

/// 毎フレーム、キューのコマンドを処理する。
/// 同一フレーム内の spawn -> set-schedule を解決できるよう、World を直接使う。
fn process_commands(world: &mut World) {
    let queue = world.resource::<CommandQueue>().0.clone();
    let pending: Vec<ScriptCommand> = std::mem::take(&mut *queue.lock().unwrap());
    for cmd in pending {
        match cmd {
            ScriptCommand::SpawnNpc { name, x, y } => {
                info!("spawn-npc: {name} ({x}, {y})");
                world
                    .spawn((
                        Npc,
                        Name::new(name.clone()),
                        Sprite::from_color(Color::srgb(0.9, 0.6, 0.2), Vec2::splat(32.0)),
                        Transform::from_xyz(x, y, 0.0),
                    ))
                    .with_children(|p| {
                        p.spawn((Text2d::new(name), Transform::from_xyz(0.0, 28.0, 1.0)));
                    });
            }
            ScriptCommand::SetSchedule { name, entries } => {
                let mut npcs = world.query_filtered::<(Entity, &Name), With<Npc>>();
                let found = npcs
                    .iter(world)
                    .find(|(_, n)| n.as_str() == name)
                    .map(|(e, _)| e);
                match found {
                    Some(e) if !entries.is_empty() => {
                        info!("set-schedule: {name} ({} 件)", entries.len());
                        world.entity_mut(e).insert(Schedule(entries));
                    }
                    Some(_) => warn!("set-schedule: {name} の日課が空です"),
                    None => warn!("set-schedule: NPC「{name}」が見つかりません"),
                }
            }
        }
    }
}

/// 現在時刻に有効な日課(時刻が現在以前の最後のエントリ。なければ前日の最後)へ一定速度で歩く。
fn follow_schedule(
    time: Res<Time>,
    clock: Res<GameClock>,
    mut npcs: Query<(&Schedule, &mut Transform), With<Npc>>,
) {
    let hour = clock.minutes / 60.0;
    for (schedule, mut tf) in &mut npcs {
        let Some(active) = schedule
            .0
            .iter()
            .rev()
            .find(|e| e.hour <= hour)
            .or(schedule.0.last())
        else {
            continue;
        };
        let here = tf.translation.truncate();
        let to = active.pos - here;
        let step = NPC_SPEED * time.delta_secs();
        let next = if to.length() <= step {
            active.pos
        } else {
            here + to.normalize() * step
        };
        tf.translation.x = next.x;
        tf.translation.y = next.y;
    }
}
