# syncplay/control

`control.rs` 的测试:

- `tests.rs`:控制权槽位、单成员换输出(#137 ③)与消息转发;
- `group_tests.rs`:多成员播放组(#137 ⑤),包括谁是主端、只转当前主端当前任期的共同计划、
  换输出时的主端交接，以及组通告发给谁。

规则全在 `../control.rs`,这里只有测试。
