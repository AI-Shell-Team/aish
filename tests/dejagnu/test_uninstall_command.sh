# DejaGnu test for aish uninstall command

# Basic test setup
set timeout 5

# Test 1: Uninstall shows information
test aish_uninstall_shows_info {
    # Run aish uninstall
    spawn aish uninstall

    expect {
        -re "(正在检测|Detecting|installation method)" {
            # Send 'n' to cancel
            send "n\r"
            pass "$testname"
        }
        timeout {
            fail "$testname: timeout"
        }
    }
}

# Test 2: Uninstall with --help shows options
test aish_uninstall_help {
    # Run aish uninstall --help
    spawn aish uninstall --help

    expect {
        -re "(purge|卸载|uninstall)" {
            pass "$testname"
        }
        timeout {
            fail "$testname: timeout"
        }
    }
}

# Run all tests
run_tests {
    aish_uninstall_shows_info
    aish_uninstall_help
}
