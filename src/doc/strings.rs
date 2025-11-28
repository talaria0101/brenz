/// wait
pub(crate) static WAIT_INFO: &str = r#"Used to pause the script execution thread for a specified number of seconds (float value), enabling timed delays in scripts. Example:
___
```gsc
for(;;) {
    wait level.regen_time - buff;
    self.health += 5;
}
```
"#;
/// thread
pub(crate) static THREAD_INFO: &str = r#"This keyword is used to call functions without blocking execution. Example:
___
```gsc
    thread my_func();
    printLn("WE ARE HERE");
```
___
The message will be printed without waiting for `my_func` to function.
"#;
/// self
pub(crate) static SELF_INFO: &str = r#"The object on which the function is being called at. Example:
___
```gsc
    player msg();

msg()
{
    self iPrintLn("hello");
}
```
___
`self` will be the player here in this example.
The default `self` object is `level`.
"#;
/// level
pub(crate) static LEVEL_INFO: &str = r#"`level` is a global variable (structured as a struct) available throughout the execution of a map or level.
Its lifespan matches the duration of the loaded level, making it ideal for map-wide state management without resetting between rounds or sessions.
___
```gsc
    level.someFlag = true;
    level thread someFunction();
```
"#;
/// game
pub(crate) static GAME_INFO: &str = r#"`game` is a global variable (structured as an array) used for general-purpose data storage during gameplay.
Its lifespan is tied to the duration of a single match or game session and it resets upon starting a new match even if the level remains loaded.
___
```gsc
    game["alliedscore"] = 0;
    game["matchstarted"] = true;
    game["attackers"] = "allies";
```
"#;
