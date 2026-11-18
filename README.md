# RustASIO

Wine/Proton용 ASIO 드라이버. Windows 프로그램(예: FMOD를 쓰는 DJMAX RESPECT V)의 ASIO 출력을 PipeWire로 직접 보냅니다. Rust로 작성했고 64비트 전용, 출력 전용입니다.

```
게임(FMOD) ──IASIO──▶ rustasio64.dll (PE, Wine builtin)
                         │ __wine_unix_call
                         ▼
                      rustasio64.so ──pw_filter (RT)──▶ PipeWire ──▶ DAC
```

- 일반 앱처럼 `pw_stream`(planar F32)으로 출력합니다. WirePlumber가 기본 장치에 자동 연결합니다.
- 게임(FMOD)이 자기 DSP 블록 크기를 ASIO 버퍼로 쓰게 하고, 그 블록을 링버퍼에 쌓습니다. PipeWire는 작은 퀀텀(기본 64)으로 꺼내 갑니다.
- 출력 지연은 대략 ASIO 버퍼 + `headroom` + `quantum`입니다.
- `sync = true`이면 PipeWire 사이클이 게임 렌더링을 기다리므로 추가 지연이 0이지만, 게임 스레드가 늦으면 xrun이 납니다.

## 빌드

```sh
nix develop -c cargo xtask dist
# → dist/lib/wine/x86_64-windows/rustasio64.dll
#   dist/lib/wine/x86_64-unix/rustasio64.so
```

`xtask`는 PE에 `Wine builtin DLL` 서명을 넣고, `.so`에서 RUNPATH를 제거합니다. 그래서 Steam Linux Runtime 컨테이너 안의 `libpipewire-0.3.so.0`을 그대로 사용합니다.

## 릴리스 받기

빌드 없이 쓰려면 GitHub Releases에서 `rustasio-<버전>-x86_64-linux.tar.gz`를 받아 압축을 풉니다. 그 안의 `scripts/install.sh`를 아래 설치 절차대로 실행하면 됩니다. 압축 파일에는 `dist/`, 스크립트, 설정 예시, README가 들어 있습니다.

## 설치 (Steam / Proton)

게임을 끈 상태에서 실행합니다.

```sh
scripts/install.sh            # 기본 APPID 960170 (DJMAX RESPECT V)
scripts/install.sh 123456     # 다른 게임
scripts/install.sh --uninstall
```

그다음 Steam 실행 옵션의 `WINEDLLPATH`를 RustASIO 경로로 바꿉니다.

```
WINEDLLPATH=$HOME/.local/share/rustasio/lib/wine PROTON_ENABLE_WAYLAND=1 mangohud %command%
```

마지막으로 게임 오디오 설정에서 ASIO를 선택합니다. 프리픽스에 PipeASIO도 등록되어 있으면 FMOD가 다른 드라이버를 고를 수 있습니다. 이때는 PipeASIO 등록을 해제하세요(`regsvr32 /u pipeasio64.dll`).

## 설정

`~/.config/rustasio/config.toml` 파일이나 환경변수로 설정합니다. 이 파일이 없으면 드라이버가 처음 열릴 때 기본값과 설명이 담긴 파일을 만듭니다. 내용은 [config.example.toml](config.example.toml)과 같고, 이미 있는 파일은 덮어쓰지 않습니다. 값을 바꾼 뒤에는 게임을 다시 시작하세요. 환경변수가 파일보다 우선합니다.

| 키 | 환경변수 | 기본값 |
|---|---|---|
| `sample_rate` | `RUSTASIO_RATE` | 48000 |
| `buffer_size` (선호 ASIO 버퍼) | `RUSTASIO_BUFFER` | 64 |
| `fixed_buffer` | `RUSTASIO_FIXED_BUFFER` | false |
| `quantum` (PipeWire 퀀텀) | `RUSTASIO_QUANTUM` | 64 |
| `headroom` (링 여유, 프레임) | `RUSTASIO_HEADROOM` | 128 |
| `outputs` | `RUSTASIO_OUTPUTS` | 2 |
| `sample_format` (`f32`/`i32`) | `RUSTASIO_FORMAT` | f32 |
| `sync` | `RUSTASIO_SYNC` | false |
| `target` | `RUSTASIO_TARGET` | (기본 싱크) |
| `auto_connect` | `RUSTASIO_AUTOCONNECT` | true |
| `rt_priority` | `RUSTASIO_RT_PRIORITY` | 80 |
| `log` | `RUSTASIO_LOG` | false |

로그는 `RUSTASIO_LOG=1 PROTON_LOG=1 %command%`로 켭니다. 출력은 `~/steam-<appid>.log`에 기록되며, 줄 앞에 `[rustasio]`가 붙습니다.

## 실시간 스케줄링 (중요)

xrun 없이 낮은 버퍼를 쓰려면 게임의 bufferSwitch 스레드와 PipeWire 스레드가 RT 우선순위를 받아야 합니다. `ulimit -r`이 0이면 둘 다 일반 스케줄링으로 돕니다. NixOS 예시:

```nix
security.rtkit.enable = true;
security.pam.loginLimits = [
  { domain = "@audio"; item = "rtprio";  type = "-"; value = "95"; }
  { domain = "@audio"; item = "memlock"; type = "-"; value = "unlimited"; }
];
users.users.<user>.extraGroups = [ "audio" ];
```

설정 후 재로그인하고 `ulimit -r`이 95인지 확인하세요. 로그에 `host thread: SCHED_FIFO 80`이 보이면 적용된 것입니다.

## 테스트

```sh
# unix 측 단독 (가짜 호스트, 작은 440Hz)
RUSTASIO_LOG=1 nix develop -c cargo run -p rustasio-unix --example sine 3

# Proton 안에서 실제 COM/ASIO 경로 (별도 테스트 프리픽스)
nix develop -c cargo build -p rustasio-testhost --target x86_64-pc-windows-gnu --release
PFX=$PWD/.testpfx scripts/proton-run.sh cmd /c ver           # 프리픽스 생성
RUSTASIO_DEST=$PWD/.testpfx/lib PFX=$PWD/.testpfx scripts/install.sh
cp target/x86_64-pc-windows-gnu/release/asiotest.exe .testpfx/pfx/drive_c/
WINEDLLPATH=$PWD/.testpfx/lib PFX=$PWD/.testpfx scripts/proton-run.sh 'C:\asiotest.exe' 5
cat .testpfx/pfx/drive_c/asiotest.log
```

실행 중에는 `pw-top`으로 RustASIO 노드의 QUANT/RATE/ERR를 확인할 수 있습니다.

## 구조

- `crates/abi`: PE와 unix 양쪽이 공유하는 `#[repr(C)]` 파라미터와 함수 코드
- `crates/pe`: COM 서버(`DllGetClassObject`, `DllRegisterServer`), IASIO, 콜백 스레드
- `crates/unix`: `__wine_unix_call_funcs`, PipeWire 엔진(futex 동기화), 설정
- `crates/testhost`: Proton에서 돌리는 최소 ASIO 호스트(`asiotest.exe`)
- `xtask`: 빌드와 후처리

## CI / 릴리스

[.github/workflows/ci.yml](.github/workflows/ci.yml)은 Nix dev shell에서 다음을 실행합니다.
- `cargo fmt --check`
- clippy (`-D warnings`, linux와 windows 타깃 모두)
- 단위 테스트
- `cargo xtask package`: dist를 빌드하고 `verify`를 통과한 뒤 tar.gz를 만듭니다.

`cargo xtask verify`가 검사하는 항목:
- DLL의 builtin 서명
- DLL export 디렉터리 이름과 COM export
- `.so`에 RUNPATH가 없는지
- `.so`의 libpipewire 의존성
- `.so`의 `__wine_unix_call_funcs`
- `.so`의 GLIBC 버전이 2.34 이하인지

릴리스 방법:
1. `Cargo.toml`의 `workspace.package.version`을 올리고 커밋합니다.
2. 같은 버전의 태그를 푸시합니다: `git tag v0.1.0 && git push origin v0.1.0`
3. CI가 태그와 버전이 일치하는지 확인한 뒤, GitHub Release에 압축 파일을 올립니다.

## 라이선스

MIT. 자세한 내용은 [LICENSE](LICENSE)를 참고하세요.

ASIO는 Steinberg Media Technologies GmbH의 상표입니다. 이 프로젝트는 ASIO SDK의 코드를 포함하지 않고, 인터페이스 정의를 독자적으로 다시 작성해 사용합니다.
