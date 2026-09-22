# Fetchpath desktop accessibility checks (FP-017, acceptance A01).
#
# Drives the real optimized executable through Windows UI Automation, the same
# tree Narrator and other Windows assistive technology reads. It asserts rather
# than describes:
#
#   1. tab order follows reading order, starting at the skip link, and the
#      Advanced options disclosure opens from the keyboard;
#   2. every keyboard-focusable control in the renderer has an accessible name;
#   3. the controls on the primary journey expose the expected role and name;
#   4. the primary journey completes with keyboard input only - typed characters
#      and Tab and Enter, delivered to the renderer, with no pointer and no
#      value-pattern shortcut - and tabbing through the composer leaves no text
#      of its own behind;
#   5. queue changes reach the polite live region;
#   6. a rejected address reaches the assertive live region, is present in the
#      accessibility tree, and is cleared again by Escape;
#   7. exactly one queue filter reports a pressed toggle state, and a filter can
#      be changed from the keyboard;
#   8. the modal shortcuts dialog takes focus, hides the page behind it from
#      assistive technology, and returns focus on Escape;
#   9. the skip link moves focus to the queue heading;
#  10. the shipped stylesheet carries the reduced-motion, forced-colors and
#      non-colour selection rules that UI Automation cannot observe.
#
# Keyboard input is delivered to the `Chrome_RenderWidgetHostHWND` window rather
# than synthesised globally with keybd_event. Global synthetic input only
# reaches the foreground window, and on a machine whose session is locked - or
# whenever any other window holds the foreground - it is silently discarded.
# Delivering to the renderer window works regardless of the foreground, and two
# routes are used, because they are not interchangeable:
#
#   * Tab goes through SendMessage (FetchpathUia::SendKey), straight to the
#     window procedure. TranslateMessage never sees it, so no WM_CHAR is
#     synthesised. Posting VK_TAB instead moves focus *and* delivers a literal
#     0x09 character, and because the character arrives after focus has already
#     moved it is typed into the field the Tab moved into. That artefact of
#     posting - not a defect of the application - previously seeded the address
#     box with a tab before the journey typed into it.
#   * Enter and Escape go through PostMessage (FetchpathUia::PostKey). Blink
#     activates a button on the keypress event, which only exists once the
#     application's message pump translates the posted key: a SendMessage Enter
#     opens neither the disclosure nor the dialog. The WM_CHAR that Escape
#     translates to is 0x1B, a control character Blink does not insert, so
#     posting it leaves no text behind.
#
# Modifier chords such as Ctrl+L cannot be exercised by either route, because
# Chromium reads the real asynchronous key state for modifiers. Ctrl+L is
# therefore asserted as a declared shortcut rather than a pressed one.
#
# Writes a JSON observation (BOM-less UTF-8) to -OutputPath and throws on any
# failure. Whether it passes or fails, the finally block restores the machine:
# the per-user application data and WebView2 profile directories are removed if
# and only if this run created them.

[CmdletBinding()]
param(
    [string] $ApplicationPath,
    [string] $StylesheetDirectory,
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'uia-common.ps1')

$repositoryRoot = Get-RepositoryRoot $PSScriptRoot
if (-not $ApplicationPath) {
    $ApplicationPath = Join-Path $repositoryRoot 'target\release\fetchpath-desktop.exe'
}
$ApplicationPath = [System.IO.Path]::GetFullPath($ApplicationPath)
if (-not (Test-Path -LiteralPath $ApplicationPath -PathType Leaf)) {
    throw "Desktop executable not found: $ApplicationPath"
}
if (-not $StylesheetDirectory) {
    $StylesheetDirectory = Join-Path $repositoryRoot 'apps\desktop\dist\assets'
}
if (-not $OutputPath) {
    $OutputPath = Join-Path $repositoryRoot 'docs\development\evidence\windows\ui-accessibility.json'
}

$workDirectory = Join-Path $repositoryRoot 'work\fp017-accessibility'
if (-not $workDirectory.StartsWith($repositoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to use a work directory outside the repository.'
}
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
[System.IO.Directory]::CreateDirectory($workDirectory) | Out-Null

# Running the application creates a per-user queue file and a WebView2 profile.
# Both are recorded now so the finally block can remove exactly what this run
# created and leave a pre-existing installation's data alone.
$appDataDirectory = Join-Path $env:APPDATA 'app.fetchpath.desktop'
$webviewProfileDirectory = Join-Path $env:LOCALAPPDATA 'app.fetchpath.desktop'
$appDataExistedBefore = Test-Path -LiteralPath $appDataDirectory
$webviewProfileExistedBefore = Test-Path -LiteralPath $webviewProfileDirectory

$VK = Get-VirtualKeys
$failures = [System.Collections.Generic.List[string]]::new()
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { $failures.Add($Message) }
}

# Roles a keyboard user actually operates. An unnamed one is announced as its
# role alone, so every one of them must carry a name.
$interactiveTypes = @('Button', 'Edit', 'ComboBox', 'CheckBox', 'RadioButton', 'Hyperlink', 'Slider', 'Spinner', 'Tab', 'TabItem')

$process = $null
$fixture = $null
$observation = [ordered]@{}

function Find-VisibleText($Root, [string] $Pattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if ($info -and $info.name -and $info.name -match $Pattern) { return $info.name }
    }
    return $null
}

function Select-Radio($Root, [string] $NamePattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if (-not $info -or $info.controlType -ne 'RadioButton') { continue }
        if ($info.name -notmatch $NamePattern) { continue }
        $info.element.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
        Start-Sleep -Milliseconds 400
        return $true
    }
    return $false
}

try {
    $fixture = Start-FixtureServer -Size (256 * 1024)
    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process
    $observation.application = $ApplicationPath
    $observation.osBuild = [string] [System.Environment]::OSVersion.Version
    $observation.foregroundWindowClass = [FetchpathUia]::ClassName([FetchpathUia]::GetForegroundWindow())
    $observation.keyboardInputMethod = [ordered]@{
        target = 'Chrome_RenderWidgetHostHWND'
        tab = 'SendMessage WM_KEYDOWN/WM_KEYUP, which TranslateMessage never sees, so no character is typed'
        enterAndEscape = 'PostMessage WM_KEYDOWN/WM_KEYUP, translated to WM_CHAR, because Blink activates on keypress'
        text = 'PostMessage WM_CHAR per character'
    }
    $observation.appDataExistedBeforeRun = $appDataExistedBefore

    # The welcome card appears only on a first run, and its two buttons sit in
    # the tab order ahead of the composer. Settings are read asynchronously
    # after the window appears, so the card is revealed a moment later than the
    # window: sampling once at launch reports "no card" for a card that shows
    # shortly after, which then shifts every tab-order step by two.
    #
    # Dismissing it is checked here as its own keyboard journey, and the
    # application is then restarted. Dismissal necessarily moves focus to the
    # Settings button, because the control the user was on has just disappeared
    # and focus must not fall to the document body; a tab-order walk started
    # from there begins one step in. `SetFocus` on the renderer's document root
    # does not clear the inner focus ring, so a relaunch is the only way back to
    # a cold document. It also verifies something worth verifying: that the
    # dismissal was persisted, and the card does not come back.
    $onboarding = $null
    $onboardingDeadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $onboarding = Find-ById $renderer 'dismiss-onboarding'
        if ($onboarding) { break }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $onboardingDeadline)
    $observation.onboardingShownOnLaunch = [bool] $onboarding
    if ($onboarding) {
        $onboarding.SetFocus()
        Start-Sleep -Milliseconds 300
        [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
        Start-Sleep -Milliseconds 1000
        $observation.onboardingDismissedByKeyboard = -not (Find-ById $renderer 'dismiss-onboarding')
        Assert-True $observation.onboardingDismissedByKeyboard `
            'Enter on the welcome card dismiss button did not close it.'

        # Restart from a cold document.
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(20000) | Out-Null
        Start-Sleep -Milliseconds 1200
        $process = Start-Process -FilePath $ApplicationPath -PassThru
        $renderer = Get-RendererRoot $process
        $renderWidget = Get-RenderWidgetHandle $process
        # Give the asynchronous settings read the same chance to reveal the card
        # that it had on the first launch. If it comes back, the dismissal did
        # not persist and every later step would be measuring the wrong layout.
        Start-Sleep -Seconds 3
        $observation.onboardingReturnedAfterRestart = [bool] (Find-ById $renderer 'dismiss-onboarding')
        Assert-True (-not $observation.onboardingReturnedAfterRestart) `
            'The welcome card came back after a restart, so dismissing it was not persisted.'
    }

    # One Tab, then wait for focus to actually move. A fixed sleep makes the
    # order check flaky: the renderer sometimes takes longer than a few hundred
    # milliseconds to process a key, and sampling too early reports the previous
    # element and shifts every later step by one.
    function Step-Tab {
        $before = Get-RendererFocus $renderer
        $beforeKey = if ($before) { "$($before.automationId)|$($before.name)" } else { '' }
        if (-not [FetchpathUia]::SendKey($renderWidget, $VK.Tab)) {
            $failures.Add('A Tab keystroke could not be delivered to the renderer window.')
        }
        $deadline = [DateTime]::UtcNow.AddSeconds(4)
        do {
            Start-Sleep -Milliseconds 120
            $after = Get-RendererFocus $renderer
            $afterKey = if ($after) { "$($after.automationId)|$($after.name)" } else { '' }
            if ($afterKey -and $afterKey -ne $beforeKey) { return $after }
        } while ([DateTime]::UtcNow -lt $deadline)
        return Get-RendererFocus $renderer
    }

    # --------------------------------------------------- 1. tab order ---------
    # Tabbing in from the document, with the disclosure opened by keyboard at
    # the point the summary receives focus.
    $observation.initialFocus = Get-RendererFocus $renderer
    $expectedOrder = @(
        @{ label = 'skip link'; name = 'Skip to the download queue'; type = 'Hyperlink' },
        @{ label = 'open-settings'; id = 'open-settings' },
        @{ label = 'keyboard-help'; id = 'keyboard-help' },
        @{ label = 'download type'; name = 'File or batch'; type = 'RadioButton' },
        @{ label = 'url'; id = 'url' },
        @{ label = 'destination'; id = 'destination' },
        @{ label = 'choose-destination'; id = 'choose-destination' },
        @{ label = 'Advanced options'; name = 'Advanced options'; openDisclosure = $true },
        @{ label = 'start time'; name = 'Start time'; type = 'Spinner' },
        @{ label = 'date picker'; id = 'picker' },
        @{ label = 'clear-schedule'; id = 'clear-schedule' },
        @{ label = 'start-download'; id = 'start-download' },
        @{ label = 'queue-search'; id = 'queue-search' },
        @{ label = 'filter All'; name = '^All$' },
        @{ label = 'filter Active'; name = '^Active$' },
        @{ label = 'filter Paused'; name = '^Paused$' },
        @{ label = 'filter Scheduled'; name = '^Scheduled$' },
        @{ label = 'filter Completed'; name = '^Completed$' },
        @{ label = 'filter Needs attention'; name = '^Needs attention$' }
    )
    $observedOrder = [System.Collections.Generic.List[object]]::new()
    foreach ($step in $expectedOrder) {
        $focused = Step-Tab
        $observedOrder.Add([pscustomobject]@{ expected = $step.label; focused = $focused })
        if (-not $focused) {
            $failures.Add("Tab order step '$($step.label)' lost keyboard focus entirely.")
            continue
        }
        $matched = $false
        if ($step.Contains('id') -and $focused.automationId -eq $step.id) { $matched = $true }
        if (-not $matched -and $step.Contains('name') -and $focused.name -and $focused.name -match $step.name) { $matched = $true }
        if ($matched -and $step.Contains('type') -and $focused.controlType -ne $step.type) { $matched = $false }
        if (-not $matched) {
            $failures.Add("Tab order expected '$($step.label)' but focus was on id='$($focused.automationId)' type='$($focused.controlType)' name='$($focused.name)'.")
        }
        if ($step.Contains('openDisclosure')) {
            [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
            Start-Sleep -Milliseconds 600
            $opened = [bool] (Find-ById $renderer 'clear-schedule')
            $observation.advancedOptionsOpenedByKeyboard = $opened
            Assert-True $opened 'Enter on the Advanced options summary did not reveal the start-time controls.'
        }
    }
    $observation.tabOrder = $observedOrder

    # ------------------------------------------ 2 + 3. names and roles --------
    # Media mode is selected so the optional media controls are checked too.
    $observation.mediaModeSelected = Select-Radio $renderer 'Video or audio'
    Assert-True $observation.mediaModeSelected 'The "Video or audio" radio button could not be selected through UI Automation.'

    $unnamed = [System.Collections.Generic.List[string]]::new()
    $inventory = [System.Collections.Generic.List[object]]::new()
    $all = Get-DescendantElements $renderer
    for ($index = 0; $index -lt $all.Count; $index++) {
        $info = Get-ElementInfo $all.Item($index)
        if (-not $info) { continue }
        if ($interactiveTypes -notcontains $info.controlType) { continue }
        if (-not $info.isKeyboardFocusable) { continue }
        $inventory.Add([pscustomobject]@{
            automationId = $info.automationId
            controlType = $info.controlType
            name = $info.name
        })
        if ([string]::IsNullOrWhiteSpace($info.name)) {
            $unnamed.Add("$($info.controlType) id='$($info.automationId)'")
        }
    }
    $observation.focusableControls = $inventory
    $observation.unnamedFocusableControls = $unnamed
    Assert-True ($unnamed.Count -eq 0) "Focusable controls without an accessible name: $($unnamed -join '; ')"

    $expectations = [System.Collections.Generic.List[object]]::new()
    $expectations += @(
        @{ id = 'url'; type = 'Edit'; name = 'Download addresses' },
        @{ id = 'destination'; type = 'Edit'; name = 'Save first item as' },
        @{ id = 'choose-destination'; type = 'Button'; name = 'destination file' },
        @{ id = 'start-download'; type = 'Button'; name = 'queue' },
        @{ id = 'keyboard-help'; type = 'Button'; name = 'Keyboard help' },
        @{ id = 'open-settings'; type = 'Button'; name = 'Settings' },
        @{ id = 'queue-search'; type = 'Edit'; name = 'Search downloads' },
        @{ id = 'clear-schedule'; type = 'Button'; name = 'Start when ready' },
        @{ id = 'active-stat'; type = 'Group'; name = 'active' },
        @{ id = 'download-kind'; type = 'Group'; name = 'Download type' },
        @{ id = 'queue-filters'; type = 'Group'; name = 'Filter downloads' },
        @{ id = 'media-options'; type = 'Group'; name = 'Video and audio options' }
    )
    # Whether the helpers are installed is a property of the machine, not of the
    # build, so the branch that is on screen decides which controls to expect.
    # Both branches must be operable; neither may be a dead end.
    $mediaToolsReady = [bool] (Find-ById $renderer 'inspect-media')
    $observation.mediaToolsReadyOnThisMachine = $mediaToolsReady
    if ($mediaToolsReady) {
        $expectations += @{ id = 'inspect-media'; type = 'Button'; name = 'Inspect link' }
        $expectations += @{ id = 'media-quality'; type = 'ComboBox'; name = 'Quality' }
    } else {
        $expectations += @{ id = 'media-open-settings'; type = 'Button'; name = 'Set them up' }
    }
    $roleAndName = [System.Collections.Generic.List[object]]::new()
    foreach ($expectation in $expectations) {
        $element = Find-ById $renderer $expectation.id
        if (-not $element) {
            $failures.Add("Control '$($expectation.id)' is not in the accessibility tree.")
            continue
        }
        $info = Get-ElementInfo $element
        if (-not $info) {
            $failures.Add("Control '$($expectation.id)' could not be read from the accessibility tree.")
            continue
        }
        $type = $info.controlType
        $name = $info.name
        $roleAndName.Add([pscustomobject]@{ automationId = $expectation.id; controlType = $type; name = $name })
        if ($type -ne $expectation.type) {
            $failures.Add("Control '$($expectation.id)' has role '$type', expected '$($expectation.type)'.")
        }
        if ($name -notmatch [regex]::Escape($expectation.name)) {
            $failures.Add("Control '$($expectation.id)' is named '$name', which does not contain '$($expectation.name)'.")
        }
    }
    $observation.primaryJourneyControls = $roleAndName

    # Ctrl+L cannot be pressed through posted messages, but it must at least be
    # announced to assistive technology as the shortcut for this field.
    $observation.addressFieldAcceleratorKey = (Find-ById $renderer 'url').Current.AcceleratorKey
    Assert-True ($observation.addressFieldAcceleratorKey -eq 'Control+L') `
        "The address field advertises accelerator '$($observation.addressFieldAcceleratorKey)', expected 'Control+L'."

    $liveRegions = [ordered]@{}
    foreach ($id in @('live-status', 'live-alert')) {
        $element = Find-ById $renderer $id
        $liveRegions[$id] = [ordered]@{
            present = [bool] $element
            controlType = if ($element) { $element.Current.ControlType.ProgrammaticName -replace '^ControlType\.', '' } else { '' }
        }
    }
    $observation.liveRegions = $liveRegions
    Assert-True ($liveRegions['live-status'].present) 'The polite live region is missing from the accessibility tree.'
    Assert-True ($liveRegions['live-alert'].present) 'The assertive live region is missing from the accessibility tree.'

    Assert-True (Select-Radio $renderer 'File or batch') 'The "File or batch" radio button could not be reselected.'

    # -------------------------------- 4 + 5. keyboard-only primary journey ----
    # From here on the composer is filled by typing. Tab moves between fields
    # and Enter submits; nothing is set through a value pattern.
    $completedPath = Join-Path $workDirectory 'keyboard-only.bin'
    $typedAddress = "$($fixture.BaseUrl)/keyboard.bin"

    # The composer must still be empty: tabbing through the two text fields in
    # the order check above moved focus and nothing else. Nothing has typed into
    # them yet, so anything here is either input the test itself leaked in or a
    # field the application populated on its own, and either one would make the
    # read-back assertion below meaningless. Escape clearing a composer that
    # holds a real address is asserted further down, on the rejected address.
    $observation.composerAfterKeyboardWalk = [ordered]@{
        url = Get-FieldValue $renderer 'url'
        destination = Get-FieldValue $renderer 'destination'
    }
    Assert-True ([string]::IsNullOrEmpty($observation.composerAfterKeyboardWalk.url) -and
        [string]::IsNullOrEmpty($observation.composerAfterKeyboardWalk.destination)) `
        ("Tabbing through the composer left text behind: url=$([regex]::Escape([string] $observation.composerAfterKeyboardWalk.url))" +
            " destination=$([regex]::Escape([string] $observation.composerAfterKeyboardWalk.destination))")

    $urlField = Find-ById $renderer 'url'
    $urlField.SetFocus()
    Start-Sleep -Milliseconds 300
    $focused = Get-RendererFocus $renderer
    Assert-True ($focused -and $focused.automationId -eq 'url') 'The address box could not be focused to begin typing.'
    [FetchpathUia]::PostText($renderWidget, $typedAddress)
    Start-Sleep -Milliseconds 400
    $typedUrlReadBack = Get-FieldValue $renderer 'url'
    $focused = Step-Tab
    Assert-True ($focused -and $focused.automationId -eq 'destination') `
        "Tab from the address box did not reach the destination. Focus: $($focused | ConvertTo-Json -Compress)"
    [FetchpathUia]::PostText($renderWidget, $completedPath)
    Start-Sleep -Milliseconds 400
    $typedDestinationReadBack = Get-FieldValue $renderer 'destination'

    $tabsToSubmit = 0
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
        $focused = Step-Tab
        $tabsToSubmit++
        if ($focused -and $focused.automationId -eq 'start-download') { break }
    }
    Assert-True ($focused -and $focused.automationId -eq 'start-download') `
        "Tabbing forward never reached the submit button. Focus: $($focused | ConvertTo-Json -Compress)"
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)

    $status = Wait-ForStatus $renderer @('Complete', 'Needs attention', 'Link needed') 90
    # The card label and the published file settle a moment after the status
    # text does, so both are waited for rather than sampled once.
    $settledCard = $null
    try {
        $settledCard = Wait-ForCondition {
            if ((Test-Path -LiteralPath $completedPath -PathType Leaf) -and
                (Get-Item -LiteralPath $completedPath).Length -eq 256 * 1024) {
                $name = Find-VisibleText $renderer 'keyboard-only\.bin, '
                if ($name) { $name } else { $null }
            } else { $null }
        } 20 'the completed download to be published and named in the queue'
    } catch { }
    $observation.keyboardOnlyJourney = [ordered]@{
        typedAddress = $typedAddress
        typedDestination = $completedPath
        typedUrlReadBack = $typedUrlReadBack
        typedDestinationReadBack = $typedDestinationReadBack
        tabsFromDestinationToSubmit = $tabsToSubmit
        finalStatus = $status
        bytesOnDisk = if (Test-Path -LiteralPath $completedPath) { (Get-Item -LiteralPath $completedPath).Length } else { 0 }
        politeLiveRegion = Get-ControlText $renderer 'live-status'
        queueCardName = $settledCard
    }
    Assert-True ($status -eq 'Complete') "The keyboard-only journey ended in '$status'."
    Assert-True ($observation.keyboardOnlyJourney.bytesOnDisk -eq 256 * 1024) `
        "The keyboard-only download wrote $($observation.keyboardOnlyJourney.bytesOnDisk) bytes."
    Assert-True ($observation.keyboardOnlyJourney.politeLiveRegion -match 'download') `
        "The polite live region did not announce the queue change. Got: '$($observation.keyboardOnlyJourney.politeLiveRegion)'"
    Assert-True ($observation.keyboardOnlyJourney.queueCardName -match 'keyboard-only\.bin, Complete') `
        "The queue card does not name its file and state. Got: '$($observation.keyboardOnlyJourney.queueCardName)'"
    Assert-True ($observation.keyboardOnlyJourney.typedUrlReadBack -eq $typedAddress) `
        "Typed characters did not reach the address box. Read back: '$($observation.keyboardOnlyJourney.typedUrlReadBack)'"
    Assert-True ($observation.keyboardOnlyJourney.typedDestinationReadBack -eq $completedPath) `
        "Typed characters did not reach the destination box. Read back: '$($observation.keyboardOnlyJourney.typedDestinationReadBack)'"

    # The fixture is the only server in this journey, so a listener that stopped
    # serving turns a working download into a connection failure. Recording its
    # health keeps a broken fixture from being reported as an application defect.
    $observation.fixtureServer = Get-FixtureDiagnostics $fixture
    Assert-True ($observation.fixtureServer.state -eq 'Running') `
        "The fixture server was not still running after the journey: $($observation.fixtureServer | ConvertTo-Json -Compress)"

    # ------------------------------------------------- 6. rejected address ----
    Set-Field $renderer 'url' 'javascript:alert(1)'
    Set-Field $renderer 'destination' (Join-Path $workDirectory 'rejected.bin')
    Start-Sleep -Milliseconds 300
    (Find-ById $renderer 'start-download').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 1500
    $observation.rejectedAddress = [ordered]@{
        assertiveLiveRegion = Get-ControlText $renderer 'live-alert'
        visibleErrorText = Find-VisibleText $renderer 'not an HTTP or HTTPS address'
    }
    Assert-True ($observation.rejectedAddress.assertiveLiveRegion -match 'not an HTTP or HTTPS address') `
        "The assertive live region did not carry the rejection. Got: '$($observation.rejectedAddress.assertiveLiveRegion)'"
    Assert-True ([bool] $observation.rejectedAddress.visibleErrorText) `
        'The rejection is not present as text in the accessibility tree.'

    $urlField = Find-ById $renderer 'url'
    $urlField.SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $VK.Escape)
    Start-Sleep -Milliseconds 900
    $observation.rejectedAddress.assertiveLiveRegionAfterEscape = Get-ControlText $renderer 'live-alert'
    $observation.rejectedAddress.visibleErrorTextAfterEscape = Find-VisibleText $renderer 'not an HTTP or HTTPS address'
    $observation.rejectedAddress.addressValueAfterEscape = Get-FieldValue $renderer 'url'
    $observation.rejectedAddress.focusAfterEscape = Get-RendererFocus $renderer
    Assert-True ([string]::IsNullOrEmpty($observation.rejectedAddress.addressValueAfterEscape)) `
        'Escape did not clear the composer.'
    Assert-True (-not $observation.rejectedAddress.visibleErrorTextAfterEscape) `
        'Escape cleared the composer but left the error text in the accessibility tree.'
    Assert-True ($observation.rejectedAddress.focusAfterEscape -and
        $observation.rejectedAddress.focusAfterEscape.automationId -eq 'start-download') `
        "Escape left focus on '$($observation.rejectedAddress.focusAfterEscape.automationId)' instead of a real control."

    # ----------------------------------------------------- 7. queue filters ---
    function Get-FilterStates {
        $states = [System.Collections.Generic.List[object]]::new()
        $items = Get-DescendantElements $renderer
        for ($index = 0; $index -lt $items.Count; $index++) {
            $info = Get-ElementInfo $items.Item($index)
            if (-not $info -or $info.controlType -ne 'Button') { continue }
            if ($info.name -notin @('All', 'Active', 'Paused', 'Scheduled', 'Completed', 'Needs attention')) { continue }
            $toggle = 'unsupported'
            try {
                $toggle = [string] $info.element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState
            } catch { }
            $states.Add([pscustomobject]@{ name = $info.name; toggleState = $toggle })
        }
        return , $states.ToArray()
    }
    $before = Get-FilterStates
    Assert-True ($before.Count -eq 6) "Expected 6 queue filters in the tree, found $($before.Count)."
    Assert-True (@($before | Where-Object { $_.toggleState -eq 'On' }).Count -eq 1) `
        "Exactly one filter must report a pressed state. Observed: $($before | ConvertTo-Json -Compress)"

    # Change the filter from the keyboard and confirm the pressed state moves.
    (Find-ById $renderer 'queue-search').SetFocus()
    Start-Sleep -Milliseconds 250
    $focused = Step-Tab
    Assert-True ($focused -and $focused.name -eq 'All') "Tab from the search box did not reach the first filter. Focus: $($focused | ConvertTo-Json -Compress)"
    $focused = Step-Tab
    Assert-True ($focused -and $focused.name -eq 'Active') "Tab did not reach the Active filter. Focus: $($focused | ConvertTo-Json -Compress)"
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Start-Sleep -Milliseconds 800
    $after = Get-FilterStates
    $observation.queueFilters = [ordered]@{ before = $before; afterKeyboardActivation = $after }
    Assert-True (@($after | Where-Object { $_.name -eq 'Active' -and $_.toggleState -eq 'On' }).Count -eq 1) `
        "Activating the Active filter from the keyboard did not set its pressed state. Observed: $($after | ConvertTo-Json -Compress)"
    Assert-True (@($after | Where-Object { $_.toggleState -eq 'On' }).Count -eq 1) `
        'More than one filter reports a pressed state after a keyboard change.'

    # -------------------------------------------------- 8. shortcuts dialog ---
    (Find-ById $renderer 'keyboard-help').SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Start-Sleep -Milliseconds 1000
    $dialog = Find-ById $renderer 'shortcuts-dialog'
    $observation.shortcutsDialog = [ordered]@{
        present = [bool] $dialog
        controlType = if ($dialog) { $dialog.Current.ControlType.ProgrammaticName -replace '^ControlType\.', '' } else { '' }
        name = if ($dialog) { $dialog.Current.Name } else { '' }
        focusOnOpen = Get-RendererFocus $renderer
        pageBehindStillReachable = [bool] (Find-ById $renderer 'url')
    }
    Assert-True ([bool] $dialog) 'Enter on Keyboard help did not open the shortcuts dialog.'
    Assert-True ($observation.shortcutsDialog.controlType -eq 'Window') `
        "The shortcuts dialog is exposed as '$($observation.shortcutsDialog.controlType)', expected a Window."
    Assert-True ($observation.shortcutsDialog.name -eq 'Keyboard shortcuts') `
        "The shortcuts dialog is named '$($observation.shortcutsDialog.name)'."
    Assert-True ($observation.shortcutsDialog.focusOnOpen -and
        $observation.shortcutsDialog.focusOnOpen.automationId -eq 'shortcuts-close') `
        "Opening the dialog did not move focus inside it. Focus: $($observation.shortcutsDialog.focusOnOpen | ConvertTo-Json -Compress)"
    Assert-True (-not $observation.shortcutsDialog.pageBehindStillReachable) `
        'The page behind the modal dialog is still reachable by assistive technology.'

    [FetchpathUia]::PostKey($renderWidget, $VK.Escape)
    Start-Sleep -Milliseconds 1000
    $observation.shortcutsDialog.presentAfterEscape = [bool] (Find-ById $renderer 'shortcuts-dialog')
    $observation.shortcutsDialog.focusAfterEscape = Get-RendererFocus $renderer
    Assert-True (-not $observation.shortcutsDialog.presentAfterEscape) 'Escape did not close the shortcuts dialog.'
    Assert-True ($observation.shortcutsDialog.focusAfterEscape -and
        $observation.shortcutsDialog.focusAfterEscape.automationId -eq 'keyboard-help') `
        "Escape did not return focus to the control that opened the dialog. Focus: $($observation.shortcutsDialog.focusAfterEscape | ConvertTo-Json -Compress)"

    # --------------------------------------------- 8b. settings dialog --------
    # The settings dialog carries the power-user controls, so it has to meet the
    # same bar as the rest: a named modal window, focus taken and returned, and
    # every control inside it named.
    (Find-ById $renderer 'open-settings').SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Start-Sleep -Milliseconds 1200
    $settings = Find-ById $renderer 'settings-dialog'
    $settingsUnnamed = [System.Collections.Generic.List[string]]::new()
    $settingsControls = [System.Collections.Generic.List[object]]::new()
    if ($settings) {
        $inside = Get-DescendantElements $settings
        for ($index = 0; $index -lt $inside.Count; $index++) {
            $info = Get-ElementInfo $inside.Item($index)
            if (-not $info) { continue }
            if ($interactiveTypes -notcontains $info.controlType) { continue }
            if (-not $info.isKeyboardFocusable) { continue }
            $settingsControls.Add([pscustomobject]@{
                automationId = $info.automationId
                controlType = $info.controlType
                name = $info.name
            })
            if ([string]::IsNullOrWhiteSpace($info.name)) {
                $settingsUnnamed.Add("$($info.controlType) id='$($info.automationId)'")
            }
        }
    }
    $observation.settingsDialog = [ordered]@{
        present = [bool] $settings
        controlType = if ($settings) { $settings.Current.ControlType.ProgrammaticName -replace '^ControlType\.', '' } else { '' }
        name = if ($settings) { $settings.Current.Name } else { '' }
        focusOnOpen = Get-RendererFocus $renderer
        pageBehindStillReachable = [bool] (Find-ById $renderer 'url')
        focusableControls = $settingsControls
        unnamedFocusableControls = $settingsUnnamed
    }
    Assert-True ([bool] $settings) 'Enter on Settings did not open the settings dialog.'
    Assert-True ($observation.settingsDialog.controlType -eq 'Window') `
        "The settings dialog is exposed as '$($observation.settingsDialog.controlType)', expected a Window."
    Assert-True ($observation.settingsDialog.name -eq 'Settings') `
        "The settings dialog is named '$($observation.settingsDialog.name)'."
    Assert-True ($observation.settingsDialog.focusOnOpen -and
        $observation.settingsDialog.focusOnOpen.automationId -eq 'settings-close') `
        "Opening Settings did not move focus inside it. Focus: $($observation.settingsDialog.focusOnOpen | ConvertTo-Json -Compress)"
    Assert-True (-not $observation.settingsDialog.pageBehindStillReachable) `
        'The page behind the modal settings dialog is still reachable by assistive technology.'
    Assert-True ($settingsUnnamed.Count -eq 0) `
        "Settings controls without an accessible name: $($settingsUnnamed -join '; ')"
    Assert-True ($settingsControls.Count -ge 10) `
        "Expected the settings dialog to expose at least 10 focusable controls, found $($settingsControls.Count)."

    [FetchpathUia]::PostKey($renderWidget, $VK.Escape)
    Start-Sleep -Milliseconds 1000
    $observation.settingsDialog.presentAfterEscape = [bool] (Find-ById $renderer 'settings-dialog')
    $observation.settingsDialog.focusAfterEscape = Get-RendererFocus $renderer
    Assert-True (-not $observation.settingsDialog.presentAfterEscape) 'Escape did not close the settings dialog.'
    Assert-True ($observation.settingsDialog.focusAfterEscape -and
        $observation.settingsDialog.focusAfterEscape.automationId -eq 'open-settings') `
        "Escape did not return focus to the Settings button. Focus: $($observation.settingsDialog.focusAfterEscape | ConvertTo-Json -Compress)"

    # ------------------------------------------------------- 9. skip link -----
    $skipLink = $null
    $all = Get-DescendantElements $renderer
    for ($index = 0; $index -lt $all.Count; $index++) {
        $info = Get-ElementInfo $all.Item($index)
        if ($info -and $info.name -eq 'Skip to the download queue') { $skipLink = $info.element; break }
    }
    Assert-True ([bool] $skipLink) 'The skip link is missing from the accessibility tree.'
    if ($skipLink) {
        $skipLink.SetFocus()
        Start-Sleep -Milliseconds 250
        [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
        Start-Sleep -Milliseconds 800
        $observation.skipLink = [ordered]@{
            controlType = $skipLink.Current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            focusAfterActivation = Get-RendererFocus $renderer
        }
        Assert-True ($observation.skipLink.focusAfterActivation -and
            $observation.skipLink.focusAfterActivation.automationId -eq 'queue-title') `
            "The skip link did not move focus to the queue heading. Focus: $($observation.skipLink.focusAfterActivation | ConvertTo-Json -Compress)"
    }

    # ------------------------------------------------- 10. stylesheet rules ---
    # UI Automation cannot see a media query, so the shipped stylesheet is read
    # directly. These rules carry motion, contrast and non-colour state, and a
    # build that dropped them would still pass every check above.
    $stylesheet = Get-ChildItem -LiteralPath $StylesheetDirectory -Filter '*.css' -File |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1
    Assert-True ([bool] $stylesheet) "No built stylesheet found in $StylesheetDirectory."
    $styleChecks = [ordered]@{}
    if ($stylesheet) {
        $css = Get-Content -LiteralPath $stylesheet.FullName -Raw
        $styleChecks = [ordered]@{
            stylesheet = $stylesheet.Name
            prefersReducedMotion = ($css -match 'prefers-reduced-motion')
            forcedColors = ($css -match 'forced-colors\s*:\s*active')
            focusVisible = ($css -match 'focus-visible')
            skipLink = ($css -match '\.skip-link')
            pressedFilterNotColourOnly = (($css -match 'aria-pressed=.?true') -and ($css -match 'double\s+canvastext'))
            highlightFocusInForcedColors = ($css -match 'solid Highlight')
            screenReaderTextNotUppercased = ($css -match '\.hero-stat \.stat-unit')
        }
        foreach ($key in @('prefersReducedMotion', 'forcedColors', 'focusVisible', 'skipLink',
                'pressedFilterNotColourOnly', 'highlightFocusInForcedColors', 'screenReaderTextNotUppercased')) {
            Assert-True ([bool] $styleChecks[$key]) "The shipped stylesheet is missing the '$key' rule."
        }
    }
    $observation.styleChecks = $styleChecks

    $observation.failures = $failures
    $observation.passed = ($failures.Count -eq 0)
} catch {
    # Keep the partial observation: a crash halfway through is still evidence.
    $observation.abortedWith = $_.Exception.Message
    $failures.Add("Run aborted: $($_.Exception.Message)")
    $observation.failures = $failures
    $observation.passed = $false
} finally {
    # Everything that touches the machine is undone here, so a failing run
    # restores state exactly as a passing one does.
    if ($process) {
        $process.Refresh()
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
        $process.WaitForExit(10000) | Out-Null
    }
    Stop-FixtureServer $fixture

    # WebView2 releases its profile directory a moment after the host process
    # exits, so removal is retried rather than attempted once.
    function Remove-Directory([string] $Path) {
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            if (-not (Test-Path -LiteralPath $Path)) { return $true }
            try { Remove-Item -LiteralPath $Path -Recurse -Force } catch { Start-Sleep -Milliseconds 500 }
        }
        return (-not (Test-Path -LiteralPath $Path))
    }

    $cleanup = [ordered]@{}
    if (-not $appDataExistedBefore -and (Test-Path -LiteralPath $appDataDirectory)) {
        $cleanup.appDataCreatedByThisRunRemoved = Remove-Directory $appDataDirectory
    }
    if (-not $webviewProfileExistedBefore -and (Test-Path -LiteralPath $webviewProfileDirectory)) {
        $cleanup.webviewProfileCreatedByThisRunRemoved = Remove-Directory $webviewProfileDirectory
    }
    Remove-Directory $workDirectory | Out-Null
    $cleanup.appDataPresentAtExit = (Test-Path -LiteralPath $appDataDirectory)
    $cleanup.webviewProfilePresentAtExit = (Test-Path -LiteralPath $webviewProfileDirectory)
    $cleanup.workDirectoryPresentAtExit = (Test-Path -LiteralPath $workDirectory)
    $cleanup.matchesPreflight = ($cleanup.appDataPresentAtExit -eq $appDataExistedBefore) -and
        ($cleanup.webviewProfilePresentAtExit -eq $webviewProfileExistedBefore) -and
        (-not $cleanup.workDirectoryPresentAtExit)
    $observation.machineRestored = $cleanup
    # $failures is the same list the observation already holds, so a restore that
    # did not complete is reported in the evidence and fails the run.
    if (-not $cleanup.matchesPreflight) {
        $failures.Add("The machine was not restored to its pre-run state: $($cleanup | ConvertTo-Json -Compress)")
        $observation.passed = $false
    }
}

[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($OutputPath)) | Out-Null
$json = $observation | ConvertTo-Json -Depth 8
# BOM-less UTF-8: Windows PowerShell's -Encoding utf8 writes a byte order mark,
# which breaks a plain JSON parse of the evidence file.
[System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
$json

if ($failures.Count -gt 0) {
    throw "Accessibility checks failed:`n - $($failures -join "`n - ")"
}
