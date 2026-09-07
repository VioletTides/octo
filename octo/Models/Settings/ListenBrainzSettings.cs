namespace Octo.Models.Settings;

/// <summary>
/// Listen submission for tracks that never touch Navidrome's library. Plays of local
/// tracks are scrobbled by Navidrome itself (Octo relays every scrobble unchanged);
/// plays of external tracks, previews from search and Continuous Radio, would
/// otherwise be lost to the listener's history. Each listener authorises with their
/// own ListenBrainz user token; a single default token covers a one-person install.
/// Every value is read through IOptionsMonitor at submit time.
/// </summary>
public class ListenBrainzSettings
{
    /// <summary>
    /// Default user token, used for any Navidrome user without an entry in
    /// <see cref="UserTokens"/>. From listenbrainz.org/settings. Empty disables
    /// submission for users without their own token.
    /// Environment variable: LISTENBRAINZ__TOKEN
    /// </summary>
    public string Token { get; set; } = "";

    /// <summary>
    /// Navidrome username to ListenBrainz user token, for installs with more than one
    /// listener. Environment variable form: LISTENBRAINZ__USERTOKENS__alice=...
    /// </summary>
    public Dictionary<string, string> UserTokens { get; set; } = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Submit a listen when an external track completes (a scrobble for an
    /// external id, or a Continuous Radio track played to the end).</summary>
    public bool SubmitExternalPlays { get; set; } = true;

    /// <summary>The token that applies to this listener, or null when none is configured.</summary>
    public string? TokenFor(string username)
    {
        if (!SubmitExternalPlays) return null;
        if (!string.IsNullOrWhiteSpace(username)
            && UserTokens.TryGetValue(username.Trim(), out var own)
            && !string.IsNullOrWhiteSpace(own))
            return own.Trim();
        return string.IsNullOrWhiteSpace(Token) ? null : Token.Trim();
    }
}
