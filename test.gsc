main()
{
    /*level.myVar = "hello world";
    maps\mp\gametype\_callback::func();
    script::func();
    myref = script::func();
    div = (4 / 2) - 3;

    array = [];
    array[0] = "a";
    array[b[a]] = 1;

    ref = ::myfunc;

    ref2 = test2::another_func;
    self.some thread foreign::func();
    level thread [[ref]]();
    [[ref]]();
    vec = (1, 2, array[1]);*/
    a = 1;
    a = a + 2;
    self thread func();
    level thread test2::another_func();
    another_func();
    thread folder\test3::func_in_test3();
    ref = folder\test3::func_in_test3;
    [[ref]]();

    for(i=0; i<player.size; i++) {}
}

// this is func
func()
{}

another_func(name, age,
             a1, b1
)
{}

//another_func()
//{}

/*
command_register(permId, name, function, description, usage)
{
    level.chatCommands[name]["permId"] = permId;
    level.chatCommands[name]["function"] = function;
    level.chatCommands[name]["description"] = description;
    level.chatCommands[name]["usage"] = usage;
    level.chatCommands_help[level.chatCommands_help.size]["name"] = name;
}

/*
func()
{
    return;
}
/

command_call(command_object)
{
    if (isDefined(level.chatCommands[command_object[0]]))
    {
        permId = level.chatCommands[command_object[0]]["permId"];
        if (!userHasPermission(self, permId))
        {
            self iPrintLn("Access denied");
            return;
        }
        [[level.chatCommands[command_object[0]]["function"]]](command_object);
    }
    else
        self iPrintLn("Unknown chat command " + command_object[0]);

    if(!isDefined(something))
        return;
    else if(test)
        return;
    else {
        iPrintLn("");
        return;
    }
}

func1()
{
    players = getPlayers();
    for(i=0; i<players.size; i++)
    {
        player = players[i];
        player thread process();
    }
}

process()
{
    self endon("disconnect");

    while(true)
    {
        self.health = 100;
        self notify("process");
    }

    switch(self.name) {
        case "^7 ^7":
        case "UnnamedPlayer":
            wait .5;
        default:
            return;
    }
}

playerDamage(eInflictor, eAttacker, iDamage, iDFlags, sMeansOfDeath, sWeapon, vPoint, vDir, sHitLoc)
{
    if(level.gametype == "bel")
    {
        if ( (isdefined (eAttacker)) && (isPlayer(eAttacker)) && (isdefined (eAttacker.god)) && (eAttacker.god == true) )
            return;

        if ( (self.sessionteam == "spectator") || (self.god == true) )
            return;
    }
    else
    {
        if(self.sessionteam == "spectator")
            return;
    }

    // Dont do knockback if the damage direction was not specified
    if(!isDefinedvDir)
        iDFlags |= level.iDFLAGS_NO_KNOCKBACK;

    if(level.gametype == "dm")
    {
        // Make sure at least one point of damage is done
        if(iDamage < 1)
            iDamage = 1;
    }
}
*/
