//! Parenthesia 最小プロトタイプ。
//! Scheme (Steel) -> コマンドキュー -> Bevy システム(毎フレーム処理) の流れ。

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

const MAIN_SCRIPT: &str = "scripts/main.scm";

/// Scheme から積まれるコマンド。ECS には触れず、データだけを持つ。
#[derive(Debug)]
enum ScriptCommand {
    SpawnNpc { name: String, x: f32, y: f32 },
}

/// Scheme 側と Bevy 側で共有するキュー。
#[derive(Resource, Clone, Default)]
struct CommandQueue(Arc<Mutex<Vec<ScriptCommand>>>);

#[derive(Component)]
struct Npc;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .init_resource::<CommandQueue>()
        .add_systems(Startup, (setup_camera, load_script))
        .add_systems(Update, process_commands)
        .run();
}

fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

/// 起動時に scripts/main.scm を読み込んで実行する。
fn load_script(queue: Res<CommandQueue>) {
    let mut engine = Engine::new();

    // Scheme に公開する API。キューに積むだけで ECS には触れない。
    let q = queue.0.clone();
    engine.register_fn("spawn-npc", move |name: String, x: f64, y: f64| {
        q.lock().unwrap().push(ScriptCommand::SpawnNpc {
            name,
            x: x as f32,
            y: y as f32,
        });
    });

    match std::fs::read_to_string(MAIN_SCRIPT) {
        Ok(src) => {
            if let Err(e) = engine.run(src) {
                error!("{MAIN_SCRIPT} の実行に失敗: {e}");
            }
        }
        Err(e) => error!("{MAIN_SCRIPT} を読み込めません: {e}"),
    }
}

/// 毎フレーム、キューのコマンドを処理する。
fn process_commands(mut commands: Commands, queue: Res<CommandQueue>) {
    let pending: Vec<ScriptCommand> = std::mem::take(&mut *queue.0.lock().unwrap());
    for cmd in pending {
        match cmd {
            ScriptCommand::SpawnNpc { name, x, y } => {
                info!("spawn-npc: {name} ({x}, {y})");
                commands
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
        }
    }
}
