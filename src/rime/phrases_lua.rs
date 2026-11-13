//! 打字侧「即时生效」翻译器（Rime Lua）
//!
//! 这里内联而不是 `include_str!`：`resources/` 整个目录不在 git，新增编译期必需文件
//! 会让新克隆直接编译失败（见 `项目简介.md` §6.5 与 `GUARD_LUA_BODY` 的同款取舍）。
//! 部署时由 `rime::write_phrases_lua` 写到用户目录 `lua\listen_phrases.lua`。
//!
//! ============================ 为什么需要它 ============================
//!
//! 设置页把词表写进 `<Rime 用户目录>\listen_hotwords.txt`，方案里由 custom_phrase 翻译器读它。
//! 但 librime 把 user_dict 缓存在**进程内**的 `UserDictionaryComponent::db_pool_`
//! （librime `src/rime/dict/user_dictionary.cc:561`），WeaselServer 起来之后就不再重读 txt。
//! 于是用户会遇到：设置页加了「快捷键 gds → 快递地址」，打字却怎么都出不来，
//! 只有重启输入法/重新部署（要 UAC）才恢复。
//!
//! 实测证据（2026-11-05，`%TEMP%\rime.weasel\rime.weasel.*.log`）：
//! ```text
//! 13:57:10 tsv.cc:18] reading tsv file: ...\listen_hotwords.txt
//! （之后同一个 WeaselServer 里开了几十个引擎，再没有第二次 reading tsv file）
//! ```
//! 而当天 16:35 写入的三条快捷输入（zjs / kmy / xjj）一直没出现在候选里。
//!
//! 这个 lua_translator 补上这条通道：每次新输入按「版本戳」（主程序写表时更新的 `.ver`，
//! 几字节）判断要不要重读词表。行为变成：改完词表 → 换一个输入框（新会话）→ 立刻出候选，
//! **不需要部署**。只读本地文件、不联网，任何异常都静默吞掉，绝不影响打字。
//!
//! 挂载方式（`schemes::schema_block` 生成 patch）：
//! ```yaml
//! engine/translators/+:
//!   - lua_translator@*listen_phrases
//! listen_phrases/path: "C:/Users/xxx/AppData/Roaming/Rime/listen_hotwords.txt"
//! listen_phrases/completion: false
//! ```

pub const BODY: &str = r##"-- 倾听输入法 · 打字侧热词 / 快捷输入（改完立刻出候选）
--
-- 位置：<Rime 用户目录>\lua\listen_phrases.lua（由倾听输入法部署时写入，勿手改）
-- 由来与实测证据见 src/rime/phrases_lua.rs 的模块注释。
--
-- 只读本地文件，不联网；任何异常都静默吞掉，绝不影响打字。

local M = {}

-- 同一编码下最多出几个候选（快捷输入的编码本来就唯一，这里主要防前缀联想刷屏）
local MAX_CAND = 10

-- 候选权重：必须压过 custom_phrase 的 initial_quality（方案里写的是 1000000）与拼音候选，
-- 这样「快捷输入」才会稳定排在第一位（与 2026-10-21 的 rank 设计一致）。
local Q_EXACT = 2000000
local Q_PREFIX = 1900000

local table_path, ver_path
local entries = {}    -- code -> { { text = "...", weight = 90 }, ... }（按权重降序）
local all_codes = {}  -- 全部编码，按「首条权重」降序（前缀联想用）
local version = nil   -- 上次读到的版本戳
local completion = false
local last_check = 0  -- 上次查版本戳的秒数（节流用）
local err_once = false

local function dbg_once(msg)
  -- 只在第一次出错时写一行，避免每帧刷盘
  if err_once or not table_path then
    return
  end
  err_once = true
  local f = io.open(table_path .. ".lua.err", "a")
  if f then
    pcall(function()
      f:write(tostring(os.time()) .. " " .. msg .. "\n")
    end)
    f:close()
  end
end

local function cfg_str(env, key)
  local v
  pcall(function()
    v = env.engine.schema.config:get_string(key)
  end)
  if v == nil or v == "" then
    return nil
  end
  return v
end

local function cfg_bool(env, key)
  local v = false
  pcall(function()
    v = env.engine.schema.config:get_bool(key)
  end)
  return v == true
end

local function read_all(path)
  local f = io.open(path, "rb")
  if not f then
    return nil
  end
  local s = f:read("*a")
  f:close()
  return s
end

-- 解析「词条 <TAB> 编码 <TAB> 权重」；# 开头是注释，其余格式问题一律跳过
local function parse(text)
  local fresh = {}
  for line in text:gmatch("[^\r\n]+") do
    if line:sub(1, 1) ~= "#" then
      local body, code, weight = line:match("^([^\t]*)\t([^\t]*)\t?([^\t]*)")
      if body and code then
        body = body:gsub("^%s+", ""):gsub("%s+$", "")
        -- 全拼缓冲只认 a-z（与 hotwords::rime_table 的写入规则一致）
        if body ~= "" and code ~= "" and code:match("^[a-z]+$") then
          local w = tonumber(weight) or 0
          local list = fresh[code]
          if not list then
            list = {}
            fresh[code] = list
          end
          local dup = false
          for _, e in ipairs(list) do
            if e.text == body then
              dup = true
              -- 同一编码同一词条：保留权重大的那个
              if w > e.weight then
                e.weight = w
              end
              break
            end
          end
          if not dup then
            list[#list + 1] = { text = body, weight = w }
          end
        end
      end
    end
  end
  -- 同编码内按权重降序（表本身已经排好，这里只是兜底）
  for _, list in pairs(fresh) do
    table.sort(list, function(a, b)
      return a.weight > b.weight
    end)
  end
  -- 编码按「该编码首条候选的权重」降序，前缀联想时先出高频的
  local codes = {}
  for code, _ in pairs(fresh) do
    codes[#codes + 1] = code
  end
  table.sort(codes, function(a, b)
    local wa = fresh[a][1] and fresh[a][1].weight or 0
    local wb = fresh[b][1] and fresh[b][1].weight or 0
    if wa ~= wb then
      return wa > wb
    end
    return a < b
  end)
  entries = fresh
  all_codes = codes
end

-- 读版本戳（主程序写表时更新）；没有版本戳时退化成「看文件长度」
local function current_version()
  if ver_path then
    local v = read_all(ver_path)
    if v and v ~= "" then
      return v
    end
  end
  return nil
end

local function ensure_loaded(force)
  if not table_path then
    return
  end
  local v = current_version()
  if (not force) and v ~= nil and v == version then
    return
  end
  local text = read_all(table_path)
  if not text then
    dbg_once("read failed: " .. tostring(table_path))
    return
  end
  local ok, err = pcall(parse, text)
  if not ok then
    dbg_once("parse failed: " .. tostring(err))
    return
  end
  -- 没有 .ver 时用「内容长度」当版本：内容改了长度基本都会变
  version = v or ("len:" .. tostring(#text))
end

local function emit(seg, text, comment, quality)
  local cand = Candidate("phrase", seg.start, seg._end, text, comment or "")
  pcall(function()
    cand.quality = quality
  end)
  yield(cand)
end

function M.init(env)
  local ns = (env.name_space or "listen_phrases"):gsub("^%*", "")
  local p = cfg_str(env, ns .. "/path")
  if not p then
    -- 兜底：按默认用户目录找（patch 没写路径时才走到这里）
    local base = os.getenv("APPDATA")
    if base then
      p = base .. "\\Rime\\listen_hotwords.txt"
    end
  end
  if p then
    table_path = p:gsub("\\", "/")
    ver_path = table_path:gsub("%.txt$", "") .. ".ver"
  end
  completion = cfg_bool(env, ns .. "/completion")
  ensure_loaded(true)
end

function M.func(input, seg, env)
  if not input or input == "" then
    return
  end
  -- 版本检查：新的一次输入（第一个字母）必查；另外每秒最多再查一次（节流）。
  -- 为什么要节流再查一次：万一是「组合串已经打到一半」时词表才更新，
  -- 只靠“第一个字母”就得等下一次输入才生效；每秒一次读的是几字节的 ver 文件，代价可忽略。
  local now = os.time()
  local empty = (next(entries) == nil)
  if #input == 1 or empty or now ~= last_check then
    last_check = now
    ensure_loaded(empty)
  end

  local code = input:lower()
  local list = entries[code]
  if list then
    local n = math.min(#list, MAX_CAND)
    for i = 1, n do
      emit(seg, list[i].text, "", Q_EXACT)
    end
    return
  end

  -- 前缀联想（对应 hotwords.prefix_completion）：编码 ≥2 位才做，避免单字母刷屏
  if not completion or #code < 2 then
    return
  end
  local n = 0
  for _, c in ipairs(all_codes) do
    if #c > #code and c:sub(1, #code) == code then
      local l = entries[c]
      if l and l[1] then
        emit(seg, l[1].text, c, Q_PREFIX)
        n = n + 1
        if n >= MAX_CAND then
          return
        end
      end
    end
  end
end

return M
"##;

#[cfg(test)]
mod tests {
    use super::BODY;

    #[test]
    fn lua_body_has_no_early_terminator() {
        // 部署出去的是纯文本 lua；Rust 的 raw string 若被内容提前截断，这里会先炸
        assert!(BODY.contains("return M"), "脚本正文不完整");
        assert!(BODY.contains("lua_translator") || BODY.contains("listen_phrases"));
        // 关键钩子函数：init / func 都必须存在，否则挂上去也不会出候选
        assert!(BODY.contains("function M.init(env)"));
        assert!(BODY.contains("function M.func(input, seg, env)"));
    }

    #[test]
    fn lua_body_balanced_quotes_and_tabs_are_escaped() {
        // 解析用 m 模式串里的 \t 必须是「反斜杠 + t」两个字（raw string 不能把它变成真制表符）
        assert!(BODY.contains("([^\\t]*)"), "制表符转义被吃掉");
        // 行首不能有 BOM/前导空白造成的解析问题
        assert!(!BODY.starts_with(char::is_whitespace));
    }
}
