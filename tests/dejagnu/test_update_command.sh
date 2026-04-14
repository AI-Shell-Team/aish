# DejaGnu test for aish update command

# Basic test setup
set timeout 5

# Test 1: Check for updates (should work even without network)
test aish_update_check {
    # Run aish update --check-only
    spawn aish update --check-only

    # Should either say "already latest" or show available versions
    expect {
        -re "(已是最新版本|有新版本可用|Checking|Already up to date|current version)" {
            pass "$testname"
        }
        timeout {
            fail "$testname: timeout"
        }
    }
}

# Test 2: Update command requires confirmation
test aish_update_requires_confirmation {
    # Run aish update
    spawn aish update

    expect {
        -re "(Continue|继续|确认)" {
            # Send 'n' to cancel
            send "n\r"
            pass "$testname"
        }
        -re "(已是最新版本|Already up to date|current version)" {
            pass "$testname"
        }
        timeout {
            fail "$testname: timeout"
        }
    }
}

# Run all tests
run_tests {
    aish_update_check
    aish_update_requires_confirmation
}
