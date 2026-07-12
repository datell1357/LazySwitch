<p align="right">
  <a href="README.md">English</a> ·
  <a href="README.ko.md"><b>한국어</b></a>
</p>

<div align="center">

# LazySwitch

로그아웃 → 로그인 반복은 그만. 한 계정이 소진되면 다음 계정이 이어받습니다.

**Codex**와 **Claude** 계정 여러 개를 로컬에 등록해두고, 사용량 창을 감시하다가 활성 계정이 바닥나는 순간 예비 계정으로 자동 전환하는 Windows 트레이 앱. 실행 중이던 CLI 세션도 이어서 되살립니다.

<p>
  <a href="#요구-사항"><img src="https://img.shields.io/badge/OS-Windows-0078D4" alt="OS Windows"></a>
  <a href="#전환-시-일어나는-일"><img src="https://img.shields.io/badge/Providers-Codex%20%7C%20Claude-111111" alt="Providers Codex and Claude"></a>
  <a href="https://github.com/datell1357/LazySwitch/releases/latest"><img src="https://img.shields.io/github/v/release/datell1357/LazySwitch?label=release&color=0A7F64" alt="Latest release"></a>
  <a href="#라이선스"><img src="https://img.shields.io/badge/License-MIT-blue" alt="License MIT"></a>
  <a href="https://github.com/datell1357/LazySwitch/stargazers"><img src="https://img.shields.io/github/stars/datell1357/LazySwitch?label=stars&labelColor=555555&color=F0B72F" alt="GitHub stars"></a>
</p>

<p>
  <a href="#cli-세션-인계"><img src="https://img.shields.io/badge/CLI-auto%20resume-5B8DEF" alt="CLI auto resume"></a>
  <a href="#명령어"><img src="https://img.shields.io/badge/command-lazyswitch%20status%20%7C%20watch-6E5BEF" alt="LazySwitch commands"></a>
</p>

<p>
  <img src="assets/accounts.png" alt="LazySwitch 계정 관리자" width="880">
</p>

<sub>
  <a href="#설치">설치</a> ·
  <a href="#처음-시작하기">처음 시작하기</a> ·
  <a href="#전환-시-일어나는-일">전환 시 일어나는 일</a> ·
  <a href="#cli-세션-인계">CLI 세션 인계</a> ·
  <a href="#설정">설정</a> ·
  <a href="#명령어">명령어</a> ·
  <a href="#경계">경계</a> ·
  <a href="#라이선스">라이선스</a>
</sub>

</div>

---

## 설치

[최신 릴리스](https://github.com/datell1357/LazySwitch/releases/latest)에서 설치 파일을 받아 실행하세요.

**[⬇ LazySwitch-Setup-0.1.0.exe](https://github.com/datell1357/LazySwitch/releases/latest)**

사용자 단위로 설치되므로 관리자 권한을 묻지 않고, 설치 후 바로 트레이에 뜹니다. 메인 창은 없고 모든 기능이 트레이 아이콘에 달려 있습니다.

서명하지 않은 앱이라 Windows SmartScreen 경고가 뜹니다. *추가 정보 → 실행*.

### 직접 빌드하기

```powershell
git clone https://github.com/datell1357/LazySwitch.git
cd LazySwitch
npm install
npm start        # 빌드 + 트레이 앱 실행
npm run dist     # release/ 에 설치 파일 생성
```

---

## 처음 시작하기

LazySwitch는 자격 증명을 묻지 않습니다. CLI가 이미 만들어둔 인증 파일을 옮길 뿐이라, 평소대로 로그인한 뒤 그 로그인을 넘겨주면 됩니다.

1. 평소처럼 계정에 **로그인** — `codex login`, 또는 `claude` 실행 후 로그인.
2. 트레이 메뉴 → **현재 로그인을 계정으로 등록** 을 눌러 이름을 붙입니다 (`Pro`, `Free`, `Spare-1` …).
3. 다음 계정으로 로그인하고 똑같이 등록합니다. 반복.
4. 트레이의 **계정** 을 열면 등록된 계정, 요금제, 각 사용량 창의 소진 정도가 보입니다.

전환이 갈 곳이 있으려면 프로바이더당 최소 2개는 등록해야 합니다.

### 계정 카드

<p>
  <img src="assets/accounts.png" alt="계정 카드" width="880">
</p>

카드에는 서로 독립적인 두 가지 사실이 나란히 표시됩니다.

| 배지 | 의미 |
|---|---|
| **사용중** | CLI와 데스크톱이 지금 이 계정으로 인증되어 있음 |
| **활성화됨** | 예비 풀에 포함 — 전환이 이 계정으로 넘어올 수 있음 |
| **비활성화됨** | 등록은 유지되지만 전환 대상에서 제외 |

둘은 독립입니다. 어떤 계정이 *사용중* 이면서 동시에 *예비로는 비활성* 일 수 있고, 카드가 그대로 보여줍니다. 배지 옆 버튼은 상태가 아니라 **동작**입니다 (**끄기** / **켜기**).

배지 아래에는 프로바이더가 알려주는 모든 사용량 창이 나옵니다 — 5시간 세션, 주간 합계, 모델별 주간, 월간 한도 — 각 막대 밑에 리셋 시각이 붙습니다.

---

## 전환 시 일어나는 일

활성 계정의 5시간 또는 주간 창이 임계값을 넘으면 LazySwitch는:

1. 살아있는 인증을 원래 슬롯에 저장합니다 (갱신된 토큰 보존).
2. 여유가 남은 다음 **활성화된** 계정을 고릅니다.
3. 그 계정의 인증을 원자적으로 라이브 인증으로 설치합니다.
4. 설정했다면 Codex 데스크톱을 재시작합니다 — 데스크톱은 옛 토큰을 메모리에 캐시하므로 파일 변경만으로는 반영되지 않습니다.
5. 설정했다면 실행 중이던 CLI 세션을 인계합니다. 아래 참고.

기본값은 전환 전에 팝업으로 확인합니다. **자동 승인** 을 켜면 무인으로 돌아갑니다.

```
~/.codex-accounts/<이름>/auth.json   등록된 Codex 계정, 각각 격리
~/.codex/auth.json                   라이브 Codex 계정 (CLI·데스크톱이 함께 읽음)

~/.claude-accounts/<이름>/           등록된 Claude 계정, 각각 격리
~/.claude/                           라이브 Claude 계정
```

사용량은 각 프로바이더 자체 텔레메트리에서 옵니다 — Codex는 최신 세션 롤아웃의 `rate_limits`, Claude는 usage API. 세션 스트림의 **사용량 초과 오류** 는 항상 반응형 백스톱이라, 퍼센트를 못 받아도 전환은 일어납니다.

---

## CLI 세션 인계

전환하면 실행 중인 모든 `codex` / `claude` CLI가 들고 있던 토큰이 무효화됩니다. **CLI 자동 재시작** 을 켜면 LazySwitch가 그 세션들을 찾아 각각 인계합니다.

1. CLI 프로세스를 종료합니다.
2. 그 CLI가 돌던 셸을 닫습니다 (닫을 수 있는 경우).
3. **새 터미널** 에서 각자의 재개 명령으로 다시 엽니다 — `codex resume <id>` / `claude --resume <id>`. 세션별로 ID를 찾아내므로 각 터미널이 자기 대화로 정확히 돌아옵니다.

탐지 범위는 PC 전체입니다. A 프로젝트의 `codex`와 B 프로젝트의 `codex`가 모두 인계되며, 각자의 디렉터리와 각자의 대화로 복원됩니다.

**Orca** 탭에서 돌던 세션은 새 Orca 탭으로 다시 열리고, 데스크톱에 콘솔 창을 흘리지 않습니다. 그 외에는 Windows Terminal 탭, 그것이 실행되지 않으면 PowerShell 창이 열립니다.

### 하지 못하는 것

| 경우 | 동작 |
|---|---|
| **진행 중이던 작업** | 손실됩니다. 재개는 대화 기록을 복원할 뿐, 실행 중이던 턴은 되살리지 못합니다. |
| **관리자 권한 터미널** | 건드리지 않습니다. 비권한 앱은 관리자 프로세스의 작업 디렉터리를 읽을 수도, 대화를 특정할 수도, 종료시킬 수도 없습니다. 이런 세션은 *수동* 으로 보고되고 재개 명령이 클립보드에 복사되므로, 해당 관리자 터미널에 직접 붙여넣으면 됩니다. |
| **터미널 에뮬레이터** | 절대 닫지 않습니다. 진짜 셸(`powershell`, `pwsh`, `cmd`, `bash`)만 닫습니다 — `wt.exe`는 무관한 탭을 함께 물고 있을 수 있습니다. |

---

## 요구 사항

**필수**

- Windows 10 / 11
- 전환을 원하는 프로바이더마다 계정 2개 이상 등록

**선택**

- Codex 데스크톱 — 전환 시 재시작을 원할 때만
- Windows Terminal — 있으면 재개된 세션이 여기서 열림. 없으면 PowerShell 창
- Orca — Orca 세션은 Orca 탭으로 재개
- Node.js 18+ — 소스에서 빌드할 때만

**불필요**

- API 키. LazySwitch는 자격 증명을 요구하지 않습니다.

---

## 설정

트레이 → **설정**, 또는 `%APPDATA%/LazySwitch/config.json` 직접 편집. 대부분 프로바이더별 설정입니다.

| 키 | 기본값 | 의미 |
|---|---|---|
| `autoApprove` | `false` | 승인 팝업 없이 전환 |
| `autoRestartCli` | `false` | 전환 시 실행 중인 CLI 세션 인계 |
| `desktopAppPath` | `""` | Codex 데스크톱 exe 또는 `shell:AppsFolder\…` AUMID (비우면 자동 탐지) |
| `primaryMinLeftPct` | `5` | 5시간 창이 이 % 이하로 남으면 전환 |
| `weeklyMinLeftPct` | `1` | 주간 창이 이 % 이하로 남으면 전환 |
| `pollIntervalSec` | Codex `30` · Claude `300` | 확인 주기 — Claude usage API는 5분 미만에서 rate-limit이 걸립니다 |
| `rateLimitEndpoint` | `""` | 선택: 신뢰 가능한 rate-limit URL |
| `language` | `""` | UI 언어: `""` 시스템, `ko` / `en` / `ja` / `zh` |

---

## 명령어

트레이 앱이 본체지만, 같은 수치를 터미널에서도 볼 수 있습니다.

```bash
lazyswitch status                    # 계정 사용량 표를 한 번 출력
lazyswitch watch --interval 30       # 표를 계속 갱신하며 표시
lazyswitch statusline                # 한 줄 요약
lazyswitch statusline claude         # 특정 프로바이더만
lazyswitch install-hooks             # Claude statusLine + Codex 빌트인 설치
lazyswitch install-codex-wrapper     # codex를 LazySwitch 사용량 패널로 감싸기
```

`install-hooks`는 Claude Code 상태 표시줄에 사용량 줄을 넣어줍니다. CLI를 벗어나지 않고도 창이 줄어드는 걸 볼 수 있습니다.

소스 체크아웃에서 설치 없이 쓰려면:

```bash
npm run build
node dist/main/cli.js status
```

---

## 경계

LazySwitch가 **하지 않는 것**:

- 사용량 제한을 우회·연장·무력화하지 않습니다 — 이미 보유한 계정들 사이를 오갈 뿐입니다
- 자격 증명을 요구·저장·전송하지 않습니다. CLI가 만든 인증 파일을 이 PC 안에서 옮길 뿐입니다
- 외부로 아무것도 보내지 않습니다. 서버도, 텔레메트리도 없고, 네트워크 호출은 각 프로바이더의 사용량 엔드포인트뿐입니다

여러 계정으로 사용량을 늘리는 것이 동의한 약관과 충돌할 수 있습니다. 판단은 사용자 몫입니다.

### 현재 상태

- 사전 감지 퍼센트는 Codex가 로컬에 `rate_limits`를 남기거나, `rateLimitEndpoint`를 지정해야 동작합니다. 오류 기반 백스톱은 어느 쪽이든 동작합니다.
- Windows 우선. macOS 경로(`desktop.ts`, 트레이)는 스텁 상태이며 검증되지 않았습니다.

---

## 라이선스

MIT. [LICENSE](LICENSE) 참고.

Codex와 Claude는 각각 OpenAI와 Anthropic의 제품입니다. LazySwitch는 독립 도구이며 두 회사와 제휴·보증·지원 관계가 없습니다.
